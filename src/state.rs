//! 运行状态：快照落盘（`state.json`）、历史环形 CSV、单实例锁。
//!
//! 三者都是「守护进程写、CLI 读」。写侧一律原子替换（见 [`crate::config::atomic_write`]），
//! 读者永远看到完整文件。

use crate::config::{atomic_write, json_num, parse_flat, Value};
use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 控制器所处阶段（也是 `status` 的 `state` 字段取值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 未充电：电流归零，控制器复位。
    Idle,
    /// 充电但温度低于介入点：给满流。
    Monitoring,
    /// 闭环限流中。
    Controlling,
    /// 守护进程未运行（只会出现在 CLI 合成状态里）。
    Stopped,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Monitoring => "monitoring",
            Phase::Controlling => "controlling",
            Phase::Stopped => "stopped",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "idle" => Phase::Idle,
            "monitoring" => Phase::Monitoring,
            "controlling" => Phase::Controlling,
            "stopped" => Phase::Stopped,
            _ => return None,
        })
    }
}

/// 一次采样快照——`state.json` 的全部字段。
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub ts: u64,
    pub uptime_s: u64,
    pub pid: u32,
    pub phase: Phase,
    pub temp: f64,
    pub current: f64,
    pub target: f64,
    pub applied_current: f64,
    pub z1: f64,
    pub z2: f64,
    pub filtered_setpoint: f64,
    pub charging: bool,
}

/// 时间戳（UNIX 秒）。系统时钟异常时回退 0，不 panic。
pub fn now_ts() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Snapshot {
    pub fn to_json(&self) -> String {
        format!(
            "{{\"ts\":{},\"uptime_s\":{},\"pid\":{},\"state\":\"{}\",\"temp\":{},\"current\":{},\
             \"target\":{},\"applied_current\":{},\"observer_z1\":{},\"observer_z2\":{},\
             \"filtered_setpoint\":{},\"charging\":{}}}",
            self.ts,
            self.uptime_s,
            self.pid,
            self.phase.as_str(),
            json_num(self.temp),
            json_num(self.current),
            json_num(self.target),
            json_num(self.applied_current),
            json_num(self.z1),
            json_num(self.z2),
            json_num(self.filtered_setpoint),
            self.charging,
        )
    }

    /// 从 `state.json` 文本解析。缺项视为损坏（返回 `None`），CLI 会退化成 `stopped`。
    pub fn parse(text: &str) -> Option<Self> {
        let pairs = parse_flat(text).ok()?;
        let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        let num = |k: &str| match get(k)? {
            Value::Num(n) if n.is_finite() => Some(n),
            _ => None,
        };
        let s = match get("state")? {
            Value::Str(s) => s,
            _ => return None,
        };
        Some(Self {
            ts: num("ts")? as u64,
            uptime_s: num("uptime_s")? as u64,
            pid: num("pid")? as u32,
            phase: Phase::parse(&s)?,
            temp: num("temp")?,
            current: num("current")?,
            target: num("target")?,
            applied_current: num("applied_current")?,
            z1: num("observer_z1")?,
            z2: num("observer_z2")?,
            filtered_setpoint: num("filtered_setpoint")?,
            charging: matches!(get("charging")?, Value::Num(n) if n != 0.0),
        })
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        atomic_write(&dir.join("state.json"), &format!("{}\n", self.to_json()))
    }

    pub fn load(dir: &Path) -> Option<Self> {
        fs::read_to_string(dir.join("state.json"))
            .ok()
            .and_then(|t| Self::parse(&t))
    }
}

/// 历史环形 CSV。内存里留全量（≤ `cap`），落盘时整体重写——点数上限几千，
/// 重写比就地截断简单且不会撕裂。
pub struct History {
    cap: usize,
    rows: RefCell<Vec<(u64, f64, f64, f64, Phase)>>,
    path: PathBuf,
}

impl History {
    /// 从磁盘载入已有历史（保留最近 `cap` 行）。
    pub fn load(dir: &Path, cap: usize) -> Self {
        let path = dir.join("history.csv");
        let mut rows = Vec::new();
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines().skip(1) {
                let f: Vec<&str> = line.split(',').collect();
                if f.len() != 5 {
                    continue;
                }
                let (Ok(t), Ok(temp), Ok(cur), Ok(tgt)) =
                    (f[0].parse::<u64>(), f[1].parse::<f64>(), f[2].parse::<f64>(), f[3].parse::<f64>())
                else {
                    continue;
                };
                let Some(ph) = Phase::parse(f[4]) else { continue };
                rows.push((t, temp, cur, tgt, ph));
            }
        }
        if rows.len() > cap {
            rows.drain(0..rows.len() - cap);
        }
        Self { cap, rows: RefCell::new(rows), path }
    }

    /// 追加一行；超过 `cap` 时从头部淘汰。返回是否落盘成功。
    pub fn push(&self, ts: u64, temp: f64, current: f64, target: f64, phase: Phase) -> Result<(), String> {
        {
            let mut r = self.rows.borrow_mut();
            // t 单调递增：同一秒内多次采样时 +1，避免 CSV 里出现非单调时间。
            let ts = r.last().map(|last| ts.max(last.0)).unwrap_or(ts);
            r.push((ts, temp, current, target, phase));
            if r.len() > self.cap {
                let over = r.len() - self.cap;
                r.drain(0..over);
            }
        }
        self.flush()
    }

    /// 整体重写 CSV。
    pub fn flush(&self) -> Result<(), String> {
        let r = self.rows.borrow();
        let mut s = String::from("t,temp,current,target,state\n");
        for (t, temp, cur, tgt, ph) in r.iter() {
            s.push_str(&format!("{t},{temp:.3},{cur:.3},{tgt:.1},{}\n", ph.as_str()));
        }
        atomic_write(&self.path, &s)
    }

    pub fn len(&self) -> usize {
        self.rows.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    /// 取最近 `n` 行（`n=0` 表示全部）。
    pub fn tail(&self, n: usize) -> Vec<(u64, f64, f64, f64, Phase)> {
        let r = self.rows.borrow();
        if n == 0 || n >= r.len() {
            r.clone()
        } else {
            r[r.len() - n..].to_vec()
        }
    }

    /// 序列化为 CLI 的 `history` JSON。
    pub fn to_json(&self, limit: usize) -> String {
        let pts = self.tail(limit);
        let mut s = format!("{{\"ok\":true,\"count\":{},\"points\":[", pts.len());
        for (i, (t, temp, cur, tgt, ph)) in pts.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"t\":{t},\"temp\":{},\"current\":{},\"target\":{},\"state\":\"{}\"}}",
                json_num(*temp),
                json_num(*cur),
                json_num(*tgt),
                ph.as_str()
            ));
        }
        s.push_str("]}");
        s
    }
}

/// 单实例锁：`create_new` 保证原子；进程退出时由 [`LockGuard::drop`] 删除。
///
/// 用 PID 内容而非 flock，是为了让 `status` 能在不持锁的情况下读出发起者。
pub struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    /// 拿到锁返回 `Ok(Some(guard))`；已有活实例返回 `Ok(None)`；
    /// 锁文件存在但进程已死（陈旧锁）则接管。
    pub fn acquire(dir: &Path) -> Result<Option<Self>, String> {
        fs::create_dir_all(dir).map_err(|e| format!("创建目录 {} 失败: {e}", dir.display()))?;
        let path = dir.join("thermald.lock");
        for _ in 0..2 {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}", std::process::id());
                    return Ok(Some(Self { path }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // 陈旧锁检测：文件里的 PID 是否还活着。
                    let alive = fs::read_to_string(&path)
                        .ok()
                        .and_then(|t| t.trim().parse::<i32>().ok())
                        .map(pid_alive)
                        .unwrap_or(false);
                    if alive {
                        return Ok(None);
                    }
                    let _ = fs::remove_file(&path);
                    continue;
                }
                Err(e) => return Err(format!("创建锁文件 {} 失败: {e}", path.display())),
            }
        }
        Err("无法获取锁（陈旧锁清理后仍冲突）".to_string())
    }

    /// 持锁进程的 PID（读锁文件内容）。
    pub fn holder_pid(dir: &Path) -> Option<u32> {
        fs::read_to_string(dir.join("thermald.lock"))
            .ok()
            .and_then(|t| t.trim().parse().ok())
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// 探测进程是否存活。`kill(pid, 0)`：成功或 `EPERM` 都算活着。
///
/// 这里直接声明 `kill`，不引入 `libc` crate（离线且依赖必须为空）。
fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if pid == std::process::id() as i32 {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        // 用 /proc 判定更稳：`kill` 需要权限，权限不足时也会 EPERM。
        if Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
        unsafe { kill(pid, 0) == 0 }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // 非 Linux（开发机）：用 `kill -0` 探测。
        std::process::Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

/// 数据目录：`THERMALD_DIR` 可覆盖（测试用）。
pub fn data_dir() -> PathBuf {
    std::env::var("THERMALD_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/data/adb/thermald"))
}
