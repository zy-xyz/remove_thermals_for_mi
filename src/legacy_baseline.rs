//! 被替换的旧模糊 PID 基线：`mi_thermald.c::fuzzy_pid_calculate` 的忠实 Rust 移植。
//!
//! # 存在理由
//! 验收③要求「与旧模糊 PID 基线的同条件表现对比」。要断言新方案更好，就必须先有一个
//! 可信的旧方案副本 —— 否则对比只是空话。因此本模块**只作为对照仪器**存在，
//! 不参与生产控制路径。
//!
//! # 移植约定
//! - 内部量纲与 C 版一致（微安），对外接口换算成安培。
//! - `read_charge_current()` 在仿真里没有 sysfs 可取，改为由调用方传入
//!   `current_actual`（真实系统里该值就是上一次下发的电流）——这是唯一的信息差异，
//!   且对旧方案有利（给了它准确的执行器反馈）。
//! - 唯一的**故意行为修正**：C 版把 `last_error` 写成函数级 `static`，导致多个控制器
//!   实例共享历史、且无法重放。这里改为结构体字段。该修正只会让旧方案表现更好。
//!
//! 原 C 版另有三处死代码已在本移植中省略（`integral` 只累加不参与输出、
//! `damping_factor` 只影响那个未被使用的积分、`get_membership` 的 `norm_x` 直通），
//! 它们对输出无贡献，保留只会制造「看起来更复杂」的假象。

use crate::model::Params;

/// 模糊语言值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FuzzyValue {
    Nb = 0,
    Nm = 1,
    Ns = 2,
    Zo = 3,
    Ps = 4,
    Pm = 5,
    Pb = 6,
}

/// 一条模糊规则给出的 PID 增量系数（模糊论域上的整数）。
#[derive(Debug, Clone, Copy)]
struct Rule {
    kp: f64,
    ki: f64,
    kd: f64,
}

const NB: f64 = -6.0;
const NM: f64 = -4.0;
const NS: f64 = -2.0;
const ZO: f64 = 0.0;
const PS: f64 = 2.0;
const PM: f64 = 4.0;
const PB: f64 = 6.0;

const fn r(kp: f64, ki: f64, kd: f64) -> Rule {
    Rule { kp, ki, kd }
}

/// 规则表，行 = e，列 = ec，与 C 版 `compact_rules[4][4]` 逐项对应。
const COMPACT_RULES: [[Rule; 4]; 4] = [
    [r(PB, NB, PB), r(PB, NM, PB), r(PB, NS, PB), r(PM, ZO, PM)],
    [r(PS, ZO, ZO), r(PS, ZO, NS), r(ZO, ZO, ZO), r(NS, PS, NM)],
    [r(ZO, PS, PS), r(ZO, PM, PS), r(ZO, PM, ZO), r(ZO, PB, ZO)],
    [r(NB, PB, NB), r(NB, PB, NM), r(NM, PB, NM), r(NM, PB, NM)],
];

/// 三角隶属函数，与 C 版同式。
fn triangle(x: f64, a: f64, b: f64, c: f64) -> f64 {
    if x <= a || x >= c {
        return 0.0;
    }
    if x <= b {
        return (x - a) / (b - a);
    }
    (c - x) / (c - b)
}

fn memberships(x: f64) -> [f64; 7] {
    [
        triangle(x, -6.0, -6.0, -4.0),
        triangle(x, -6.0, -4.0, -2.0),
        triangle(x, -4.0, -2.0, 0.0),
        triangle(x, -2.0, 0.0, 2.0),
        triangle(x, 0.0, 2.0, 4.0),
        triangle(x, 2.0, 4.0, 6.0),
        triangle(x, 4.0, 6.0, 6.0),
    ]
}

fn rule_index(v: FuzzyValue) -> usize {
    match v {
        FuzzyValue::Nb | FuzzyValue::Nm | FuzzyValue::Ns => 0,
        FuzzyValue::Zo => 1,
        FuzzyValue::Ps => 2,
        FuzzyValue::Pm | FuzzyValue::Pb => 3,
    }
}

fn from_index(i: usize) -> FuzzyValue {
    match i {
        0 => FuzzyValue::Nb,
        1 => FuzzyValue::Nm,
        2 => FuzzyValue::Ns,
        3 => FuzzyValue::Zo,
        4 => FuzzyValue::Ps,
        5 => FuzzyValue::Pm,
        _ => FuzzyValue::Pb,
    }
}

/// 旧模糊 PID 控制器。量纲：内部微安，接口安培。
#[derive(Debug, Clone)]
pub struct LegacyFuzzyPid {
    kp_base: f64,
    ki_base: f64,
    kd_base: f64,
    setpoint: f64,
    intervention_temp: f64,
    overtemp_temp: f64,
    hardware_limit_temp: f64,
    integral: f64,
    prev_error: f64,
    last_error: f64,
    output_min: f64,
    output_max: f64,
    e_range: f64,
    ec_range: f64,
    current_actual: f64,
}

impl LegacyFuzzyPid {
    /// 按 C 版 `fuzzy_pid_init` 初始化；`u0` 为初始电流（A）。
    pub fn new(params: &Params, u0: f64) -> Self {
        Self {
            kp_base: 200_000.0,
            ki_base: 25_000.0,
            kd_base: 60_000.0,
            setpoint: params.setpoint,
            intervention_temp: params.setpoint - 2.0,
            overtemp_temp: params.setpoint + 0.3,
            hardware_limit_temp: 48.0,
            integral: 0.0,
            prev_error: 0.0,
            last_error: 0.0,
            output_min: 0.0,
            output_max: 24_000_000.0,
            e_range: 0.3,
            ec_range: 0.3,
            current_actual: u0 * 1e6,
        }
    }

    /// 模糊推理给出的 PID 增量系数。
    fn fuzzy_deltas(&self, e: f64, ec: f64) -> (f64, f64, f64) {
        let norm_e = (e / self.e_range) * 6.0;
        let norm_ec = (ec / self.ec_range) * 6.0;
        let em = memberships(norm_e);
        let ecm = memberships(norm_ec);

        let (mut kp, mut ki, mut kd, mut w_total) = (0.0, 0.0, 0.0, 0.0);
        for (i, wi) in em.iter().enumerate() {
            for (j, wj) in ecm.iter().enumerate() {
                let weight = wi * wj;
                if weight > 1e-6 {
                    let rule = COMPACT_RULES[rule_index(from_index(i))][rule_index(from_index(j))];
                    kp += weight * rule.kp;
                    ki += weight * rule.ki;
                    kd += weight * rule.kd;
                    w_total += weight;
                }
            }
        }
        if w_total > 1e-6 {
            (
                kp / w_total * 150_000.0,
                ki / w_total * 10_000.0,
                kd / w_total * 20_000.0,
            )
        } else {
            (0.0, 0.0, 0.0)
        }
    }

    /// 一步控制。`current_temp` 为测量温度（℃），`dt` 为采样周期（s），
    /// `current_actual` 为执行器当前实际电流（A）。返回要下发的电流（A）。
    pub fn step(&mut self, current_temp: f64, dt: f64, current_actual: f64) -> f64 {
        self.current_actual = current_actual * 1e6;

        let error = current_temp - self.setpoint;
        let error_delta = (error - self.prev_error) / dt;

        // 超温快速降温保护。
        let mut damping_factor = 1.0;
        if current_temp >= self.overtemp_temp && error_delta > 0.1 {
            damping_factor = 0.2;
        }

        let (kp_d, ki_d, kd_d) = self.fuzzy_deltas(error, error_delta);
        let kp = (self.kp_base + kp_d).clamp(180_000.0, 350_000.0);
        let ki = (self.ki_base + ki_d).clamp(0.0, 8_000.0);
        let kd = (self.kd_base + kd_d).clamp(20_000.0, 40_000.0);

        // 积分分离：低于介入点才积分，超温区停止积分。
        if error < (self.setpoint - self.intervention_temp) {
            self.integral += error * dt * (1.0 - damping_factor * 0.5);
            self.integral = self.integral.clamp(-40.0, 40.0);
        }

        // 增量式。
        let delta_error = error - self.last_error;
        let mut adjustment =
            kp * delta_error + ki * error * dt + kd * (error - 2.0 * self.prev_error + self.last_error);

        // 超温直接惩罚：每超 0.1℃ 减 2A。
        if error > 0.0 {
            adjustment -= error * 20_000_000.0;
        }

        let mut new_output = self.current_actual + adjustment;

        // 软限制：超温点之上按线性斜坡收敛到硬件保护温度。
        if current_temp > self.overtemp_temp {
            let mut max_allowed = self.output_max * (self.hardware_limit_temp - current_temp)
                / (self.hardware_limit_temp - self.overtemp_temp);
            if max_allowed < 0.0 {
                max_allowed = 0.0;
            }
            if new_output > max_allowed {
                new_output = max_allowed;
            }
        }

        new_output = new_output.clamp(self.output_min, self.output_max);

        // 历史值更新：顺序与 C 版一致（last_error 取更新前的 prev_error）。
        self.last_error = self.prev_error;
        self.prev_error = error;

        new_output / 1e6
    }
}
