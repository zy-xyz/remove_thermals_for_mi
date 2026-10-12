//! 守护进程主循环。
//!
//! 每 `sample_period` 秒：读温度与充电状态 → 决定阶段 → 算电流 → 写 sysfs →
//! 落盘快照与历史。配置支持热加载（改 `config.json` 后无需重启，≤1 个周期生效）。
//!
//! 本模块**不实施硬过温保护**：硬件保护由充电驱动负责，这里只靠闭环把温度压在
//! `hardware_limit` 以下。见 `CONTRACT.md` §0 与 `control.rs` 的说明。

use crate::config::Config;
use crate::control::ImcAdrc;
use crate::device::{Charging, Device};
use crate::state::{data_dir, now_ts, History, LockGuard, Phase, Snapshot};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// 收到 SIGTERM/SIGINT 时置位，主循环据此收尾（删锁文件、落盘终态）。
static STOP: AtomicBool = AtomicBool::new(false);

/// 安装信号处理器。只置一个原子标志——信号处理器里不做任何非异步安全的事。
#[cfg(unix)]
fn install_signal_handlers() {
    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    extern "C" fn on_signal(_sig: i32) {
        STOP.store(true, Ordering::SeqCst);
    }
    let h = on_signal as *const () as usize;
    // SIGINT=2, SIGTERM=15
    unsafe {
        signal(2, h);
        signal(15, h);
    }
}

#[cfg(not(unix))]
fn install_signal_handlers() {}

/// 入口。返回进程退出码。
pub fn run() -> i32 {
    let dir = data_dir();
    let lock = match LockGuard::acquire(&dir) {
        Ok(Some(g)) => g,
        // 已有活实例：**退出码 0**，不留孤儿（契约 §7）。
        Ok(None) => {
            eprintln!("thermald: 已有实例在运行（pid {:?}），本进程退出", LockGuard::holder_pid(&dir));
            return 0;
        }
        Err(e) => {
            eprintln!("thermald: {e}");
            return 3;
        }
    };

    let cfg = match Config::load_or_init(&dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("thermald: 配置无效: {e}");
            return 1;
        }
    };

    install_signal_handlers();
    let dev = Device::from_env();
    let started = Instant::now();
    let pid = std::process::id();

    // 电流初值：真实设备上无法可靠回读，取 0 起步（首步会按闭环立即给出正确的值）。
    let mut ctrl: Option<ImcAdrc> = None;
    let mut cfg_now = cfg;
    let mut hist = History::load(&dir, cfg_now.csv_max_points as usize);

    while !STOP.load(Ordering::SeqCst) {
        let period = cfg_now.sample_period.clamp(1.0, 10.0);
        let tick = Instant::now();

        // 热加载：只在配置真的变了才重建控制器。
        if let Some(new) = Config::load_quiet(&dir) {
            if new != cfg_now {
                eprintln!("thermald: 配置热加载生效");
                let params_changed = new.to_params() != cfg_now.to_params();
                cfg_now = new;
                if params_changed {
                    // 参数变了就重建控制器：ESO/预估器都按旧参数标定，沿用会给出错的
                    // 扰动估计。代价是一次暂态，换来参数立即真实生效。
                    ctrl = None;
                }
                hist = History::load(&dir, cfg_now.csv_max_points as usize);
            }
        }

        let charging = dev.read_charging() == Some(Charging::Yes);
        let params = cfg_now.to_params();
        let meas = dev.read_temp();
        // 电池侧实测电流：此刻读数反映**上一拍写入**的实际后果（CV 阶段会低于设定值）。
        // 控制器必须用它推进 ESO/预估器，否则会按一个从未发生的输入推算状态。
        // 读不到写 NaN → JSON `null`（前端画断点），喂控制器时按 0 保守处理。
        let actual = dev.read_current_now();
        let u_meas = actual.unwrap_or(0.0);

        let (phase, u, applied) = match (charging, meas) {
            (true, Some(y)) => {
                let c = match ctrl.as_mut() {
                    Some(c) => c,
                    None => {
                        // 起步电流：低于介入点给满流，否则从 0 起步让闭环自己爬。
                        let u0 = if y < params.intervention_temp() { params.current_max } else { 0.0 };
                        ctrl.insert(ImcAdrc::new(&params, y, u0, period))
                    }
                };
                let u = c.step_with_meas(y, params.setpoint, period, u_meas);
                // 阶段判定：仍在介入点以下且尚未被限流 → 监控态（满流）。
                let ph = if y < params.intervention_temp() && u >= params.current_max - 1e-9 {
                    Phase::Monitoring
                } else {
                    Phase::Controlling
                };
                if let Err(e) = dev.write_current(u) {
                    eprintln!("thermald: {e}");
                }
                (ph, u, u)
            }
            // 未充电（或温度读不到）：电流归零并复位控制器，避免带着旧状态重新介入。
            _ => {
                if ctrl.is_some() {
                    ctrl = None;
                }
                if let Err(e) = dev.write_current(0.0) {
                    eprintln!("thermald: {e}");
                }
                (Phase::Idle, 0.0, 0.0)
            }
        };

        let (z1, z2, rf) = match ctrl.as_ref() {
            Some(c) => {
                let (a, b) = c.observer();
                (a, b, c.filtered_setpoint())
            }
            None => (meas.unwrap_or(0.0), 0.0, params.setpoint),
        };

        let actual = actual.unwrap_or(f64::NAN);

        let snap = Snapshot {
            ts: now_ts(),
            uptime_s: started.elapsed().as_secs(),
            pid,
            phase,
            temp: meas.unwrap_or(f64::NAN),
            current: applied,
            target: params.setpoint,
            applied_current: applied,
            actual_current: actual,
            z1,
            z2,
            filtered_setpoint: rf,
            charging,
        };
        if let Err(e) = snap.save(&dir) {
            eprintln!("thermald: 快照落盘失败: {e}");
        }
        if let Some(y) = meas {
            if let Err(e) = hist.push(snap.ts, y, applied, params.setpoint, actual, phase) {
                eprintln!("thermald: 历史落盘失败: {e}");
            }
        }

        // 干跑模式：处理完当前样本再前进一步；已在末尾就退出，否则会以满速空转
        // （既烧 CPU 又永不返回）。
        if dev.is_dry() {
            if !dev.advance() {
                break;
            }
            continue;
        }
        dev.advance();

        // 补齐到整数个采样周期。
        let spent = tick.elapsed();
        let want = Duration::from_secs_f64(period);
        if spent < want {
            sleep_interruptible(want - spent);
        }
        let _ = u;
    }

    // 收尾：写一份 stopped 快照，锁由 Drop 删除。
    let _ = Snapshot {
        ts: now_ts(),
        uptime_s: started.elapsed().as_secs(),
        pid,
        phase: Phase::Stopped,
        temp: f64::NAN,
        current: 0.0,
        target: cfg_now.setpoint,
        applied_current: 0.0,
        actual_current: f64::NAN,
        z1: 0.0,
        z2: 0.0,
        filtered_setpoint: cfg_now.setpoint,
        charging: false,
    }
    .save(&dir);
    eprintln!("thermald: 已停止");
    drop(lock);
    0
}

/// 分片睡眠，保证信号到达后能快速退出（最长等 200ms）。
fn sleep_interruptible(d: Duration) {
    let mut left = d;
    let slice = Duration::from_millis(200);
    while left > Duration::ZERO && !STOP.load(Ordering::SeqCst) {
        let s = left.min(slice);
        std::thread::sleep(s);
        left -= s;
    }
}

/// 判断目录里是否有活实例（供 CLI `status` 用）。
pub fn is_running(dir: &Path) -> Option<u32> {
    let pid = LockGuard::holder_pid(dir)?;
    Snapshot::load(dir).filter(|s| s.pid == pid).map(|s| s.pid)
}
