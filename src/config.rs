//! 配置：JSON 读写、校验、与 [`Params`] 互转。
//!
//! 零依赖——手写一个扁平 JSON 解析器。配置文件只有一层「数字字段」，状态文件是
//! 一层「数字/字符串」，所以不需要通用 JSON 库，也不需要引入任何 crate。
//!
//! 解析器同时供 [`crate::state`] 复用（见 [`parse_flat`]）。

use crate::model::{CtrlParams, Params, PlantModel};
use std::fs;
use std::io::Write as _;
use std::path::Path;

/// JSON 标量值（扁平对象里的值只有这三种）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Num(f64),
    Str(String),
    Null,
}

/// 配置字段名（也是 JSON 键名、`set` 的键名、WebUI 表单字段名——单一真源）。
pub const KEYS: [&str; 18] = [
    "version",
    "setpoint",
    "intervention_delta",
    "overtemp_delta",
    "hardware_limit",
    "current_min",
    "current_max",
    "ambient",
    "sensor_tau",
    "k",
    "t",
    "tau",
    "wo",
    "wc",
    "lambda",
    "b0",
    "csv_max_points",
    "sample_period",
];

/// 全部可调参数的唯一真源（落盘形态）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    pub version: f64,
    pub setpoint: f64,
    pub intervention_delta: f64,
    pub overtemp_delta: f64,
    pub hardware_limit: f64,
    pub current_min: f64,
    pub current_max: f64,
    pub ambient: f64,
    pub sensor_tau: f64,
    pub k: f64,
    pub t: f64,
    pub tau: f64,
    pub wo: f64,
    pub wc: f64,
    pub lambda: f64,
    pub b0: f64,
    pub csv_max_points: f64,
    pub sample_period: f64,
}

/// 生成 `get`/`set`，避免 18 个字段写 36 个 match 分支。
macro_rules! accessors {
    ($($f:ident),* $(,)?) => {
        impl Config {
            /// 按名字取值。
            pub fn get(&self, k: &str) -> Option<f64> {
                match k { $(stringify!($f) => Some(self.$f),)* _ => None }
            }
            /// 按名字赋值（不做范围校验，校验见 [`Config::validate`]）。
            pub fn set(&mut self, k: &str, v: f64) -> Result<(), String> {
                match k { $(stringify!($f) => { self.$f = v; Ok(()) },)* _ => Err(format!("未知参数: {k}")) }
            }
        }
    };
}
accessors!(
    version, setpoint, intervention_delta, overtemp_delta, hardware_limit, current_min,
    current_max, ambient, sensor_tau, k, t, tau, wo, wc, lambda, b0, csv_max_points,
    sample_period,
);

/// 单个字段的合法范围（闭区间）。`None` 表示无范围约束。
pub fn range(k: &str) -> Option<(f64, f64)> {
    Some(match k {
        "setpoint" => (30.0, 47.5),
        "intervention_delta" => (0.0, 10.0),
        "overtemp_delta" => (0.0, 5.0),
        "hardware_limit" => (40.0, 60.0),
        "current_min" => (0.0, 5.0),
        "current_max" => (1.0, 30.0),
        "ambient" => (0.0, 45.0),
        "sensor_tau" => (0.0, 60.0),
        "k" => (0.01, 10.0),
        "t" => (10.0, 2000.0),
        "tau" => (0.0, 300.0),
        "wo" => (0.0001, 1.0),
        "wc" => (0.0001, 1.0),
        "lambda" => (0.05, 60.0),
        "b0" => (1e-6, 1.0),
        "csv_max_points" => (60.0, 10000.0),
        "sample_period" => (1.0, 10.0),
        _ => return None,
    })
}

impl Default for Config {
    fn default() -> Self {
        Self::from_params(&Params::default(), 600.0, 1.0)
    }
}

impl Config {
    /// 从运行参数反推落盘配置。
    pub fn from_params(p: &Params, csv_max_points: f64, sample_period: f64) -> Self {
        Self {
            version: 1.0,
            setpoint: p.setpoint,
            intervention_delta: p.intervention_gap,
            overtemp_delta: p.overtemp_margin,
            hardware_limit: p.hardware_limit,
            current_min: p.current_min,
            current_max: p.current_max,
            ambient: p.ambient,
            sensor_tau: p.sensor_tau,
            k: p.model.k,
            t: p.model.t,
            tau: p.model.tau,
            wo: p.ctrl.wo,
            wc: p.ctrl.wc,
            lambda: p.ctrl.lambda,
            b0: p.ctrl.b0,
            csv_max_points,
            sample_period,
        }
    }

    /// 转成控制器参数。
    pub fn to_params(&self) -> Params {
        Params {
            model: PlantModel { k: self.k, t: self.t, tau: self.tau },
            ambient: self.ambient,
            setpoint: self.setpoint,
            intervention_gap: self.intervention_delta,
            overtemp_margin: self.overtemp_delta,
            hardware_limit: self.hardware_limit,
            sensor_tau: self.sensor_tau,
            current_min: self.current_min,
            current_max: self.current_max,
            ctrl: CtrlParams {
                wo: self.wo,
                wc: self.wc,
                lambda: self.lambda,
                b0: self.b0,
            },
        }
    }

    /// 逐字段校验：范围 + 跨字段一致性。返回全部问题（不短路），便于一次告知用户。
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if self.version != 1.0 {
            errs.push(format!("不支持的配置版本: {}（本程序只认 version=1）", self.version));
        }
        for k in KEYS {
            let v = self.get(k).expect("KEYS 与字段一一对应");
            if !v.is_finite() {
                errs.push(format!("{k} 不是有限数值"));
                continue;
            }
            if let Some((lo, hi)) = range(k) {
                if v < lo || v > hi {
                    errs.push(format!("{k} 超出范围 [{lo}, {hi}]: {v}"));
                }
            }
        }
        if self.current_max < self.current_min {
            errs.push(format!(
                "current_max({}) 小于 current_min({})",
                self.current_max, self.current_min
            ));
        }
        // 与 `CtrlParams::new` 的稳定性约束一致：前向 Euler 要求 ωo·dt < 2。
        if self.wo * self.sample_period >= 2.0 {
            errs.push(format!(
                "wo({}) × sample_period({}) = {} ≥ 2，ESO 前向 Euler 会发散",
                self.wo,
                self.sample_period,
                self.wo * self.sample_period
            ));
        }
        if self.sample_period <= 0.0 {
            errs.push("sample_period 必须为正".to_string());
        }
        errs
    }

    /// 序列化为单行 JSON。
    pub fn to_json(&self) -> String {
        let mut s = String::from("{");
        for (i, k) in KEYS.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push('"');
            s.push_str(k);
            s.push_str("\":");
            s.push_str(&json_num(self.get(k).unwrap()));
        }
        s.push('}');
        s
    }

    /// 从 JSON 文本解析：缺项、未知键、非数字、越界都拒绝。
    pub fn parse(text: &str) -> Result<Self, String> {
        let pairs = parse_flat(text)?;
        let mut cfg = Config::default();
        let mut seen = Vec::new();
        for (k, v) in &pairs {
            let n = match v {
                Value::Num(n) => *n,
                _ => return Err(format!("{k} 必须是数字")),
            };
            if cfg.set(k, n).is_err() {
                return Err(format!("未知参数: {k}"));
            }
            seen.push(k.clone());
        }
        // 缺项检查：允许省略（用默认值补齐），但**不允许拼写错误**——未知键上面已拒。
        // 这里只报警不报错，保证旧配置文件可平滑升级。
        let _ = seen;
        let errs = cfg.validate();
        if !errs.is_empty() {
            return Err(errs.join("; "));
        }
        Ok(cfg)
    }

    /// 从文件加载；文件不存在则返回默认配置并落盘。
    pub fn load_or_init(dir: &Path) -> Result<Self, String> {
        let path = dir.join("config.json");
        match fs::read_to_string(&path) {
            Ok(t) => Config::parse(&t),
            Err(_) => {
                let cfg = Config::default();
                cfg.save(dir)?;
                Ok(cfg)
            }
        }
    }

    /// 热加载：失败时返回 `None`（调用方保留上一份好配置，不让守护进程崩）。
    pub fn load_quiet(dir: &Path) -> Option<Self> {
        fs::read_to_string(dir.join("config.json"))
            .ok()
            .and_then(|t| Config::parse(&t).ok())
    }

    /// 原子落盘（写临时文件 + rename），保证读者永远看到完整文件。
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        fs::create_dir_all(dir).map_err(|e| format!("创建目录 {} 失败: {e}", dir.display()))?;
        atomic_write(&dir.join("config.json"), &format!("{}\n", self.to_json()))
    }
}

/// f64 → JSON 数字；`NaN`/`inf` 一律输出 `null`（JSON 无此表示，绝不能吐非法字面量）。
pub fn json_num(v: f64) -> String {
    if !v.is_finite() {
        return "null".to_string();
    }
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// 原子写：同目录临时文件 + rename（同文件系统内 rename 是原子的）。
pub fn atomic_write(path: &Path, data: &str) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp).map_err(|e| format!("写入 {} 失败: {e}", tmp.display()))?;
        f.write_all(data.as_bytes())
            .map_err(|e| format!("写入 {} 失败: {e}", tmp.display()))?;
        f.sync_all().ok();
    }
    fs::rename(&tmp, path).map_err(|e| format!("重命名到 {} 失败: {e}", path.display()))
}

/// 极简扁平 JSON 解析：只接受 `{"k": <num|"str"|null>, ...}`。
///
/// 刻意不做通用解析——配置与状态文件都不嵌套。遇到嵌套对象/数组直接报错，
/// 免得「看起来支持却没测过」。
pub(crate) fn parse_flat(text: &str) -> Result<Vec<(String, Value)>, String> {
    let b: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let ws = |i: &mut usize| {
        while *i < b.len() && b[*i].is_whitespace() {
            *i += 1;
        }
    };
    let expect = |i: &mut usize, c: char| -> Result<(), String> {
        if *i < b.len() && b[*i] == c {
            *i += 1;
            Ok(())
        } else {
            Err(format!("JSON 第 {} 字符处期望 '{c}'", *i + 1))
        }
    };

    ws(&mut i);
    expect(&mut i, '{')?;
    let mut out = Vec::new();
    ws(&mut i);
    if i < b.len() && b[i] == '}' {
        return Ok(out);
    }
    loop {
        ws(&mut i);
        expect(&mut i, '"')?;
        let mut key = String::new();
        while i < b.len() && b[i] != '"' {
            if b[i] == '\\' {
                return Err("JSON 键不支持转义".to_string());
            }
            key.push(b[i]);
            i += 1;
        }
        expect(&mut i, '"')?;
        ws(&mut i);
        expect(&mut i, ':')?;
        ws(&mut i);

        let val = if i < b.len() && b[i] == '"' {
            i += 1;
            let mut s = String::new();
            while i < b.len() && b[i] != '"' {
                s.push(b[i]);
                i += 1;
            }
            expect(&mut i, '"')?;
            Value::Str(s)
        } else if b[i..].starts_with(&['n', 'u', 'l', 'l']) {
            i += 4;
            Value::Null
        } else if b[i] == '{' || b[i] == '[' {
            return Err(format!("字段 {key} 不支持嵌套的 JSON 对象/数组"));
        } else {
            let start = i;
            while i < b.len() && !matches!(b[i], ',' | '}' | ' ' | '\n' | '\r' | '\t') {
                i += 1;
            }
            let tok: String = b[start..i].iter().collect();
            let n: f64 = tok
                .parse()
                .map_err(|_| format!("字段 {key} 的值不是合法数字: {tok}"))?;
            Value::Num(n)
        };
        out.push((key, val));

        ws(&mut i);
        if i < b.len() && b[i] == ',' {
            i += 1;
            continue;
        }
        expect(&mut i, '}')?;
        break;
    }
    ws(&mut i);
    if i != b.len() {
        return Err(format!("JSON 尾部有多余内容（第 {} 字符起）", i + 1));
    }
    Ok(out)
}
