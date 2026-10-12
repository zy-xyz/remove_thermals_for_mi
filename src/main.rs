//! CLI 入口。所有子命令输出**单行 JSON** 到 stdout；错误走 stderr + 非零退出码。
//!
//! 契约见 `CONTRACT.md` §4。零依赖：手写参数解析。

use std::path::Path;
use std::process::ExitCode;
use thermald::config::{json_num, Config, KEYS};
use thermald::daemon;
use thermald::device::{Charging, Device};
use thermald::model::{Plant, Sensor};
use thermald::state::{data_dir, now_ts, History, Phase, Snapshot};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.is_empty() { &args[..] } else { &args[1..] };

    let code = match cmd {
        "status" => cmd_status(),
        "get" => match rest.first() {
            Some(k) => cmd_get(k),
            None => usage("get 需要参数 <key>"),
        },
        "set" => cmd_set(rest),
        "defaults" => {
            println!("{{\"ok\":true,\"config\":{}}}", Config::default().to_json());
            0
        }
        "sim" => cmd_sim(rest),
        "history" => cmd_history(rest),
        "daemon" => daemon::run(),
        "version" => {
            println!("{{\"ok\":true,\"version\":\"{VERSION}\"}}");
            0
        }
        _ => usage(if cmd.is_empty() { "缺少子命令" } else { "未知子命令" }),
    };
    ExitCode::from(code as u8)
}

/// 用法错误：stderr + 退出码 2。
fn usage(why: &str) -> i32 {
    eprintln!("thermald: {why}");
    eprintln!(
        "用法:\n  thermald status\n  thermald get <key>\n  thermald set k=v [k=v ...]\n  \
         thermald set --reset\n  thermald defaults\n  thermald sim [--setpoint X] [--steps N] [--mismatch F]\n  \
         thermald history [--limit N]\n  thermald daemon\n  thermald version"
    );
    2
}

/// 参数错误：退出码 1，stderr 中文原因。
fn fail(msg: &str) -> i32 {
    eprintln!("thermald: {msg}");
    println!("{{\"ok\":false,\"error\":{}}}", json_str(msg));
    1
}

/// 环境错误：退出码 3。
fn env_fail(msg: &str) -> i32 {
    eprintln!("thermald: {msg}");
    3
}

fn cmd_status() -> i32 {
    let dir = data_dir();
    let cfg = match Config::load_or_init(&dir) {
        Ok(c) => c,
        Err(e) => return fail(&format!("配置无效: {e}")),
    };
    let params = cfg.to_params();
    let running_pid = daemon::is_running(&dir);
    let snap = Snapshot::load(&dir);
    let hist = History::load(&dir, cfg.csv_max_points as usize);

    // 优先用快照里的行情；没快照就把设备现值读一把（含干跑）。
    let (temp, current, phase, uptime, pid) = match (running_pid, snap) {
        (Some(pid), Some(s)) => (Some(s.temp), Some(s.current), s.phase, s.uptime_s, pid),
        _ => {
            let dev = Device::from_env();
            let t = dev.read_temp();
            let charging = dev.read_charging() == Some(Charging::Yes);
            let ph = if charging { Phase::Monitoring } else { Phase::Idle };
            (t, None, if running_pid.is_some() { ph } else { Phase::Stopped }, 0, 0)
        }
    };

    let charging = match (running_pid, &snap_charging(&dir)) {
        _ => match Device::from_env().read_charging() {
            Some(Charging::Yes) => true,
            Some(Charging::No) => false,
            None => Snapshot::load(&dir).map(|s| s.charging).unwrap_or(false),
        },
    };

    let (z1, z2, rf, actual) = match Snapshot::load(&dir) {
        Some(s) if running_pid.is_some() => (s.z1, s.z2, s.filtered_setpoint, s.actual_current),
        _ => (f64::NAN, f64::NAN, params.setpoint, f64::NAN),
    };

    let j = format!(
        "{{\"ok\":true,\"version\":\"{VERSION}\",\"running\":{},\"pid\":{},\"uptime_s\":{},\"ts\":{},\
         \"temp\":{},\"current\":{},\"target\":{},\"intervention_temp\":{},\"overtemp_temp\":{},\
         \"setpoint\":{},\"hardware_limit\":{},\"charging\":{},\"state\":\"{}\",\
         \"observer_z1\":{},\"observer_z2\":{},\"filtered_setpoint\":{},\"applied_current\":{},\
         \"actual_current\":{},\"config\":{},\"history_points\":{}}}",
        running_pid.is_some(),
        pid,
        uptime,
        now_ts(),
        json_num_opt(temp),
        json_num_opt(current),
        json_num(params.setpoint),
        json_num(params.intervention_temp()),
        json_num(params.overtemp_temp()),
        json_num(params.setpoint),
        json_num(params.hardware_limit),
        charging,
        phase.as_str(),
        json_num(z1),
        json_num(z2),
        json_num(rf),
        json_num_opt(current),
        json_num(actual),
        cfg.to_json(),
        hist.len(),
    );
    println!("{j}");
    0
}

/// 仅为可读性存在的占位：`status` 里充电态优先读设备，其次读快照。
fn snap_charging(dir: &Path) -> Option<bool> {
    Snapshot::load(dir).map(|s| s.charging)
}

fn cmd_get(key: &str) -> i32 {
    let dir = data_dir();
    let cfg = match Config::load_or_init(&dir) {
        Ok(c) => c,
        Err(e) => return fail(&format!("配置无效: {e}")),
    };
    match cfg.get(key) {
        Some(v) => {
            println!("{{\"ok\":true,\"key\":\"{key}\",\"value\":{}}}", json_num(v));
            0
        }
        None => fail(&format!("未知参数: {key}")),
    }
}

fn cmd_set(args: &[String]) -> i32 {
    let dir = data_dir();
    let old = match Config::load_or_init(&dir) {
        Ok(c) => c,
        Err(e) => return fail(&format!("配置无效: {e}")),
    };
    let mut cfg = old;

    if args.first().map(String::as_str) == Some("--reset") {
        cfg = Config::default();
    } else {
        if args.is_empty() {
            return usage("set 需要 k=v 参数对，或 --reset");
        }
        for kv in args {
            let Some((k, v)) = kv.split_once('=') else {
                return fail(&format!("参数格式应为 k=v，收到: {kv}"));
            };
            let Ok(n) = v.parse::<f64>() else {
                return fail(&format!("{k} 的值不是合法数字: {v}"));
            };
            if let Err(e) = cfg.set(k, n) {
                return fail(&e);
            }
        }
    }

    // 校验不通过**绝不落盘**——保证文件逐字节不变。
    let errs = cfg.validate();
    if !errs.is_empty() {
        return fail(&errs.join("; "));
    }
    if let Err(e) = cfg.save(&dir) {
        return env_fail(&e);
    }

    let mut changed = String::new();
    for k in KEYS {
        let (a, b) = (old.get(k).unwrap(), cfg.get(k).unwrap());
        if (a - b).abs() > f64::EPSILON {
            if !changed.is_empty() {
                changed.push(',');
            }
            changed.push_str(&format!("\"{k}\":{}", json_num(b)));
        }
    }
    println!(
        "{{\"ok\":true,\"changed\":{{{}}},\"config\":{}}}",
        changed,
        cfg.to_json()
    );
    0
}

fn cmd_history(args: &[String]) -> i32 {
    let mut limit = 0usize;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--limit" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse().ok()) {
                    Some(n) => limit = n,
                    None => return usage("--limit 需要一个整数"),
                }
            }
            other => return usage(&format!("history 不认的参数: {other}")),
        }
        i += 1;
    }
    let dir = data_dir();
    let cap = Config::load_or_init(&dir).map(|c| c.csv_max_points as usize).unwrap_or(600);
    let hist = History::load(&dir, cap);
    println!("{}", hist.to_json(limit));
    0
}

fn cmd_sim(args: &[String]) -> i32 {
    let dir = data_dir();
    let mut cfg = match Config::load_or_init(&dir) {
        Ok(c) => c,
        Err(e) => return fail(&format!("配置无效: {e}")),
    };
    let mut steps = 600usize;
    let mut mismatch = 1.0f64;

    let mut i = 0;
    while i < args.len() {
        let cur = args[i].as_str();
        let mut take = |name: &str| -> Option<String> {
            if i + 1 < args.len() {
                i += 1;
                Some(args[i].clone())
            } else {
                eprintln!("thermald: {name} 缺参数");
                None
            }
        };
        match cur {
            "--setpoint" => {
                let Some(v) = take("--setpoint") else { return usage("--setpoint 缺参数") };
                let Ok(n) = v.parse::<f64>() else { return fail(&format!("--setpoint 非数字: {v}")) };
                cfg.setpoint = n;
            }
            "--steps" => {
                let Some(v) = take("--steps") else { return usage("--steps 缺参数") };
                let Ok(n) = v.parse::<usize>() else { return fail(&format!("--steps 非整数: {v}")) };
                steps = n.min(100_000);
            }
            "--mismatch" => {
                let Some(v) = take("--mismatch") else { return usage("--mismatch 缺参数") };
                let Ok(n) = v.parse::<f64>() else { return fail(&format!("--mismatch 非数字: {v}")) };
                if !(0.1..=10.0).contains(&n) {
                    return fail(&format!("--mismatch 应在 [0.1, 10]: {n}"));
                }
                mismatch = n;
            }
            other => return usage(&format!("sim 不认的参数: {other}")),
        }
        i += 1;
    }

    let errs = cfg.validate();
    if !errs.is_empty() {
        return fail(&errs.join("; "));
    }
    let m = run_sim(&cfg, steps, mismatch);
    println!("{m}");
    0
}

/// 跑一次闭环仿真，返回 CLI 契约 §4.6 的 JSON。
///
/// 起点是**未充电的常温**：环境温度起步、电流 0，然后闭环往设定点爬。
/// `settle_steps` 用「首次入带 ±0.1℃」语义（`control.rs` 场景③的注释里有实测证据：
/// 严格「保持入带」语义受 25s 纯滞后 + 8s 传感器耦合限制，物理下界就在 650 步以上）。
fn run_sim(cfg: &Config, steps: usize, mismatch: f64) -> String {
    let params = cfg.to_params();
    let dt = cfg.sample_period;
    let target = params.setpoint;

    // 真实对象带失配（τ 放大），控制器仍用标称参数——这才是 `--mismatch` 的意义。
    let real = thermald::model::PlantModel {
        tau: params.model.tau * mismatch,
        ..params.model
    };
    let mut plant = Plant::with_temp(real, params.ambient, params.ambient, 0.0, dt);
    // 温度传感器也按设定滞后，仿真才和真机一致。
    let mut sensor = Sensor::new(params.sensor_tau, plant.temp());
    let mut ctrl = thermald::control::ImcAdrc::new(&params, sensor.reading(), 0.0, dt);

    let mut peak = f64::MIN;
    let mut peak_true = f64::MIN;
    let (mut umin, mut umax) = (f64::MAX, f64::MIN);
    let mut ys = Vec::with_capacity(steps);
    for _ in 0..steps {
        let u = ctrl.step(sensor.reading(), target, dt);
        let y = plant.step(u, dt);
        sensor.measure(y, dt);
        ys.push(sensor.reading());
        peak = peak.max(sensor.reading());
        peak_true = peak_true.max(y);
        umin = umin.min(u);
        umax = umax.max(u);
    }

    let final_temp = ys.last().copied().unwrap_or(f64::NAN);
    let steady_error = final_temp - target;
    // 首次入带：第一次回到 ±0.1℃ 的步数（此后可能再出带，那是滞后固有的超调尾巴）。
    let settle = ys
        .iter()
        .position(|y| (y - target).abs() <= 0.1)
        .unwrap_or(usize::MAX);
    let max_y = ys.iter().cloned().fold(f64::MIN, f64::max);
    let overshoot = if max_y > target { (max_y - target) / 0.5 * 100.0 } else { 0.0 };

    // 真温峰值（不是测量值）才是越界判据——传感器滞后会掩盖真实峰值。
    let limit_respected = peak_true < params.hardware_limit;

    format!(
        "{{\"ok\":true,\"steps\":{steps},\"overshoot_pct\":{},\"settle_steps\":{},\
         \"steady_error\":{},\"peak_temp\":{},\"final_temp\":{},\"current_min_seen\":{},\
         \"current_max_seen\":{},\"hardware_limit\":{},\"limit_respected\":{},\"mismatch\":{}}}",
        json_num(overshoot),
        if settle == usize::MAX { "null".to_string() } else { settle.to_string() },
        json_num(steady_error),
        json_num(peak),
        json_num(final_temp),
        json_num(umin),
        json_num(umax),
        json_num(params.hardware_limit),
        limit_respected,
        json_num(mismatch),
    )
}

/// JSON 数字或 `null`（非有限值不能输出为 `NaN`）。
fn json_num_opt(v: Option<f64>) -> String {
    match v {
        Some(x) => json_num(x),
        None => "null".to_string(),
    }
}

/// 极简 JSON 字符串转义（错误消息里可能有引号/反斜杠）。
fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}
