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
    /// 电池侧实测电流（A），只读展示用；读不到为 NaN（JSON 里是 `null`）。
    pub actual_current: f64,
    pub z1: f64,
    pub z2: f64,
    pub filtered_setpoint: f64,
    pub charging: bool,
}

/// CSV 单元格数值：NaN 写成空字段（列位置仍保留，读回时按 NaN 处理）。
fn csv_num(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.3}")
    } else {
        String::new()
    }
}

/// 时间戳（UNIX 秒）。系统时钟异常时回退 0，不 panic。
pub fn now_ts() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Snapshot {
    pub fn to_json(&self) -> String {
        format!(
            "{{\"ts\":{},\"uptime_s\":{},\"pid\":{},\"state\":\"{}\",\"temp\":{},\"current\":{},\
             \"target\":{},\"applied_current\":{},\"actual_current\":{},\"observer_z1\":{},\"observer_z2\":{},\
             \"filtered_setpoint\":{},\"charging\":{}}}",
            self.ts,
            self.uptime_s,
            self.pid,
            self.phase.as_str(),
            json_num(self.temp),
            json_num(self.current),
            json_num(self.target),
            json_num(self.applied_current),
            json_num(self.actual_current),
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
            // 新增字段：旧版快照没有它，缺失不算损坏（否则整个快照被判死，
            // `is_running` 返回 None → 面板假报「未运行」）。
            actual_current: num("actual_current").unwrap_or(f64::NAN),
            z1: num("observer_z1")?,
            z2: num("observer_z2")?,
            filtered_setpoint: num("filtered_setpoint")?,
            charging: match get("charging")? {
                Value::Bool(b) => b,
                // 兼容旧版快照里的数字编码（0/1）。
                Value::Num(n) => n != 0.0,
                _ => return None,
            },
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
    rows: RefCell<Vec<(u64, f64, f64, f64, f64, Phase)>>,
    path: PathBuf,
}

impl History {
    /// 从磁盘载入已有历史（保留最近 `cap` 行）。
    /// 兼容旧版 5 列 CSV（无实际电流列，`actual` 补 NaN）。
    pub fn load(dir: &Path, cap: usize) -> Self {
        let path = dir.join("history.csv");
        let mut rows = Vec::new();
        if let Ok(text) = fs::read_to_string(&path) {
            for line in text.lines().skip(1) {
                let f: Vec<&str> = line.split(',').collect();
                if f.len() != 5 && f.len() != 6 {
                    continue;
                }
                let (Ok(t), Ok(temp), Ok(cur), Ok(tgt)) =
                    (f[0].parse::<u64>(), f[1].parse::<f64>(), f[2].parse::<f64>(), f[3].parse::<f64>())
                else {
                    continue;
                };
                let actual = f.get(4).filter(|_| f.len() == 6).and_then(|s| s.parse::<f64>().ok());
                let state_col = if f.len() == 6 { f[5] } else { f[4] };
                let Some(ph) = Phase::parse(state_col) else { continue };
                rows.push((t, temp, cur, tgt, actual.unwrap_or(f64::NAN), ph));
            }
        }
        if rows.len() > cap {
            rows.drain(0..rows.len() - cap);
        }
        Self { cap, rows: RefCell::new(rows), path }
    }

    /// 追加一行；超过 `cap` 时从头部淘汰。返回是否落盘成功。
    pub fn push(
        &self,
        ts: u64,
        temp: f64,
        current: f64,
        target: f64,
        actual: f64,
        phase: Phase,
    ) -> Result<(), String> {
        {
            let mut r = self.rows.borrow_mut();
            // t 单调递增：同一秒内多次采样时 +1，避免 CSV 里出现非单调时间。
            let ts = r.last().map(|last| ts.max(last.0)).unwrap_or(ts);
            r.push((ts, temp, current, target, actual, phase));
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
        let mut s = String::from("t,temp,current,target,actual,state\n");
        for (t, temp, cur, tgt, actual, ph) in r.iter() {
            s.push_str(&format!(
                "{t},{temp:.3},{cur:.3},{tgt:.1},{},{}\n",
                csv_num(*actual),
                ph.as_str()
            ));
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
    pub fn tail(&self, n: usize) -> Vec<(u64, f64, f64, f64, f64, Phase)> {
        let r = self.rows.borrow();
        if n == 0 || n >= r.len() {
            r.clone()
        } else {
            r[r.len() - n..].to_vec()
        }
    }

    /// 序列化为 CLI 的 `history` JSON。`actual` 为 NaN 时写 `null`。
    pub fn to_json(&self, limit: usize) -> String {
        let pts = self.tail(limit);
        let mut s = format!("{{\"ok\":true,\"count\":{},\"points\":[", pts.len());
        for (i, (t, temp, cur, tgt, actual, ph)) in pts.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"t\":{t},\"temp\":{},\"current\":{},\"target\":{},\"actual\":{},\"state\":\"{}\"}}",
                json_num(*temp),
                json_num(*cur),
                json_num(*tgt),
                json_num(*actual),
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
                    // 陈旧锁检测：读锁文件里的 PID，看进程是否还活着。
                    //
                    // 空内容别立刻判死：`create_new` 与写入 PID 之间有窗口，并发启动时
                    // 后来者若把空文件当陈旧锁抢走，就会出现两个守护进程同时写
                    // `state.json.tmp`，rename 报 ENOENT。先等一下再读，仍为空才接管。
                    let holder = |p: &Path| {
                        fs::read_to_string(p).ok().and_then(|t| t.trim().parse::<i32>().ok())
                    };
                    if holder(&path).is_none() {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    if holder(&path).map(pid_alive).unwrap_or(false) {
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
///
/// 未设置时从自身可执行文件位置反推：`<模块目录>/bin/thermald` → `<模块目录>/data`。
/// 不硬编码模块目录名，因为 KernelSU 的模块目录名未必等于 module.prop 的 id
/// （实测机型上目录名是仓库名，而 id 是 thermald）。
/// 正常由 service.sh 显式传 THERMALD_DIR，此兜底只用于手工执行二进制的场景。
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("THERMALD_DIR") {
        return PathBuf::from(dir);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|bin| bin.with_file_name("data")))
        .unwrap_or_else(|| PathBuf::from("/data/adb/thermald"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(charging: &str) -> String {
        format!(
            "{{\"ts\":1,\"uptime_s\":2,\"pid\":3,\"state\":\"idle\",\"temp\":35.6,\"current\":0,\
             \"target\":47,\"applied_current\":0,\"observer_z1\":35.6,\"observer_z2\":0,\
             \"filtered_setpoint\":47,\"charging\":{charging}}}"
        )
    }

    /// 回归：守护进程写出的 `charging` 是 JSON 布尔，解析器必须认；
    /// 曾因只认数字导致整份快照解析失败、`status` 恒报 `running:false`。
    #[test]
    fn snapshot_parses_json_bool_charging() {
        assert!(!Snapshot::parse(&snap("false")).expect("false 应可解析").charging);
        assert!(Snapshot::parse(&snap("true")).expect("true 应可解析").charging);
    }

    /// 回归：锁文件里是活着的 PID 时必须让位，绝不能抢锁。
    /// 曾因空锁文件被误判为陈旧锁，导致两个守护进程并发写 state.json.tmp、rename 报 ENOENT。
    #[test]
    fn lock_defers_to_live_holder() {
        let dir = std::env::temp_dir().join(format!("thermald-lk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let lock = dir.join("thermald.lock");
        // 自己就是活着的持有者
        fs::write(&lock, format!("{}\n", std::process::id())).unwrap();
        assert!(LockGuard::acquire(&dir).unwrap().is_none(), "活锁不该被抢");
        assert!(lock.exists(), "不该删掉别人的锁");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 崩溃残留的空锁文件要能接管，否则守护进程再也起不来。
    #[test]
    fn lock_reclaims_empty_stale_file() {
        let dir = std::env::temp_dir().join(format!("thermald-lk2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("thermald.lock"), "").unwrap();
        assert!(LockGuard::acquire(&dir).unwrap().is_some(), "空陈旧锁应被接管");
        let _ = fs::remove_dir_all(&dir);
    }

    /// 旧版快照用数字编码，仍需兼容。
    #[test]
    fn snapshot_accepts_legacy_numeric_charging() {
        assert!(!Snapshot::parse(&snap("0")).expect("0 应可解析").charging);
        assert!(Snapshot::parse(&snap("1")).expect("1 应可解析").charging);
    }
}
