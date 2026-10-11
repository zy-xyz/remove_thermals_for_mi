//! 被控对象模型、参数唯一真源、开环仿真与阶跃辨识。
//!
//! # 单位约定
//! - 电流 `u`：安培（A）。sysfs 的 µA 由 IO 层换算，不进模型。
//! - 温度 `y`：摄氏度（℃）。
//! - 时间：秒（s）。
//!
//! # 标称对象
//! 一阶惯性 + 纯滞后（FOPDT）：
//!
//! ```text
//! G(s) = K · e^(-τs) / (T·s + 1)
//! ```
//!
//! 时域上，温度相对环境基线的偏差 `Δ = y - ambient` 满足
//! `T·dΔ/dt = K·u(t-τ) - Δ`，即满流稳态温升为 `K·u`。
//!
//! **本文件是全部可调参数的唯一存放处**；控制律、IO 层、WebUI 均从 [`Params`] 取值。

/// FOPDT 标称对象参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlantModel {
    /// 稳态增益 K：单位电流引起的稳态温升，(℃/A)。
    pub k: f64,
    /// 热时间常数 T：达到最终温升 63.2% 所需时间（s）。
    pub t: f64,
    /// 纯滞后 τ：从施加电流到温度开始响应的时间（s）。
    pub tau: f64,
}

impl PlantModel {
    /// 由 K/T/τ 构造；三者必须为有限正数（τ 允许为 0）。
    pub fn new(k: f64, t: f64, tau: f64) -> Option<Self> {
        let ok = k.is_finite() && t.is_finite() && tau.is_finite() && k > 0.0 && t > 0.0 && tau >= 0.0;
        ok.then_some(Self { k, t, tau })
    }

    /// 满流下的稳态温度：`ambient + K·u`。
    pub fn steady_temp(&self, ambient: f64, u: f64) -> f64 {
        ambient + self.k * u
    }
}

/// 全部可调参数的唯一真源。
///
/// 阈值语义沿用旧 C 实现（`mi_thermald.c`）并改为可配置：
/// 设定点 47.0℃、介入点 = 设定点 − 2.0℃、超温点 = 设定点 + 0.3℃、硬件上限 48℃、电流上限 24A。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    /// 标称对象模型。
    pub model: PlantModel,
    /// 环境温度基线（℃），开环仿真的起点与渐近基线。
    pub ambient: f64,
    /// 目标温度（℃）。
    pub setpoint: f64,
    /// 介入温差（℃）：温度低于 `setpoint - gap` 时直接给满流。
    pub intervention_gap: f64,
    /// 超温余量（℃）：`setpoint + margin` 以上进入软斜坡限流区。
    pub overtemp_margin: f64,
    /// 硬件过温上限（℃）：充电驱动的硬保护阈值，本模块**不实现**保护，只把温度压在
    /// 此线以下（闭环设计约束）。
    pub hardware_limit: f64,
    /// 温度传感器一阶热耦合时间常数（s）。仿真用它把真实温度滞后成测量温度。
    pub sensor_tau: f64,
    /// 电流下限（A）。
    pub current_min: f64,
    /// 电流上限（A）。
    pub current_max: f64,
    /// IMC-ADRC 控制器参数。
    pub ctrl: CtrlParams,
}

/// IMC-ADRC 控制器参数（唯一真源，WebUI 可调）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CtrlParams {
    /// 观测器带宽 ωo（rad/s）。ESO 离散用 `β1 = 2ωo`、`β2 = ωo²`。
    pub wo: f64,
    /// 控制器带宽 ωc（rad/s）。比例项 `u0 = ωc·(r − z1)`。
    pub wc: f64,
    /// IMC 设定点滤波器时间常数 λ（s）：`1/(λs+1)`。
    pub lambda: f64,
    /// 输入增益标称值 b0（℃/(A·s)）。
    ///
    /// 由开环斜率定标：`b0 = K / T`，默认 = 1.05/180 ≈ 0.0058333。
    /// 见 [`Params::default`] 的派生注释。
    pub b0: f64,
}

impl Default for Params {
    fn default() -> Self {
        // 由 K/T 推导带宽：一阶对象的转折频率即 1/T。
        let model = PlantModel { k: 1.05, t: 180.0, tau: 25.0 };
        let w0 = 1.0 / model.t;
        let ctrl = CtrlParams {
            // ωo = ωc = 7.75/T = 0.04306 rad/s。
            //
            // 观测器带宽**刻意不高于**控制器带宽：扫描实测（τ 真实 +50% 失配、wc/wo 网格
            // 扫描）失配超调几乎只由 ωo/ωc 比值决定——比值越大越差。默认的旧值 ωo=60/T
            // （比值 7.5）在真失配下超调 34.0%，远超规格；取 ωo=ωc 后降到 13.6%。
            // 物理上原因清楚：Smith 修正的模型失配残差会被高增益 z2 放大成超调
            // （同一效应也让 ωo ≥ 200/T 时标称稳态下垂到 2~9℃）。
            //
            // 代价是标称调节时间从 80 步涨到 110 步（规格 120 步），余量仅 8%——这是
            // 「失配鲁棒」换「标称速度」的自觉取舍，不要再把 ωo 调大。
            wo: 7.75 * w0,
            // ωc = 7.75/T：与 ωo 同比锁定。再快则失配超调超 15%，再慢则标称调节时间越过 120 步。
            wc: 7.75 * w0,
            // λ = 1s：设定点整形。λ 太小等于不滤波，太大会拖慢调节时间。
            lambda: 1.0,
            // b0 = K/T：开环斜率。K·u/T 即 (℃/s)。刻意独立于 model，见 control.rs 模块注释。
            b0: model.k / model.t,
        };
        Self {
            // 工程起点：满流 24A 时开环稳态约 50.2℃，高于设定点 47.0℃ 与硬件上限 48℃，
            // 因此必须闭环限流；τ=25s 对应实测「温度上升有延时」的量级。
            model,
            ambient: 25.0,
            setpoint: 47.0,
            intervention_gap: 2.0,
            overtemp_margin: 0.3,
            // 充电驱动的硬保护线：设定点 47.0℃ 与它留 1℃ 余量，闭环把峰值压在 47.02℃
            // （40→47 起升实测），扰动 0.9℃ 时峰值 47.91℃，均不越线。
            hardware_limit: 48.0,
            // 传感器热耦合 8s：NTC 贴片与电芯间热阻的典型量级，约 1/3 的 τ。
            sensor_tau: 8.0,
            current_min: 0.0,
            current_max: 24.0,
            ctrl,
        }
    }
}

impl Params {
    /// 介入温度：低于此温度直接给满流。
    pub fn intervention_temp(&self) -> f64 {
        self.setpoint - self.intervention_gap
    }

    /// 超温点：高于此温度进入软斜坡限流。
    pub fn overtemp_temp(&self) -> f64 {
        self.setpoint + self.overtemp_margin
    }

    /// 把电流钳位到 `[current_min, current_max]`。
    pub fn clamp_current(&self, u: f64) -> f64 {
        u.clamp(self.current_min, self.current_max)
    }
}

impl CtrlParams {
    /// 观测器增益 β1 = 2ωo。ESO：`e = z1 − y`，`z1' = z2 − β1·e + b0·u`。
    pub fn beta1(&self) -> f64 {
        2.0 * self.wo
    }

    /// 观测器增益 β2 = ωo²。ESO：`z2' = −β2·e`。
    pub fn beta2(&self) -> f64 {
        self.wo * self.wo
    }

    /// 喂给 ESO 的 `b0·u`：用**实际施加**的 u（已限幅），避免限幅污染观测器。
    pub fn b0u(&self, u_actual: f64) -> f64 {
        self.b0 * u_actual
    }
}

/// 开环对象仿真器：FOPDT 离散（前向 Euler + 环形滞后缓冲）。
#[derive(Debug, Clone)]
pub struct Plant {
    model: PlantModel,
    ambient: f64,
    y: f64,
    delay: DelayLine,
}

impl Plant {
    /// 在环境温度 `ambient` 处静止起步，`u0` 为阶跃前电流，`dt` 为采样周期（s）。
    ///
    /// `dt` 必须与后续 [`Plant::step`] 的 `dt` 一致：滞后缓冲长度 `round(τ/dt)` 在建线时
    /// 定死，运行期不重建。
    pub fn new(model: PlantModel, ambient: f64, u0: f64, dt: f64) -> Self {
        Self { model, ambient, y: model.steady_temp(ambient, u0), delay: DelayLine::new(model.tau, dt, u0) }
    }

    /// 指定初始温度起步（可为非平衡态）。
    pub fn with_temp(model: PlantModel, ambient: f64, y0: f64, u0: f64, dt: f64) -> Self {
        let mut p = Self::new(model, ambient, u0, dt);
        p.y = y0;
        p
    }

    /// 当前温度（℃）。
    pub fn temp(&self) -> f64 {
        self.y
    }

    /// 推进 `dt` 秒，施加电流 `u`，返回新温度。
    pub fn step(&mut self, u: f64, dt: f64) -> f64 {
        self.step_with_disturbance(u, dt, 0.0)
    }

    /// 同上，但叠加恒定外部热扰动率 `w`（℃/s）。
    ///
    /// 物理含义是持续的外部热源（如边充边跑游戏），控制器的 ESO 应把它当总扰动估掉。
    pub fn step_with_disturbance(&mut self, u: f64, dt: f64, w: f64) -> f64 {
        let u_eff = self.delay.push(u);
        let d = self.model.k * u_eff - (self.y - self.ambient);
        self.y += dt * (d / self.model.t + w);
        self.y
    }

    /// 注入瞬时外部热扰动：温度直接抬高 `delta`（℃）。
    ///
    /// 用于「扰动恢复」回归：控制器只看到温度跳变，必须靠自身拉回设定点。
    pub fn perturb(&mut self, delta: f64) {
        self.y += delta;
    }
}

/// 温度传感器一阶热耦合滞后：把真实温度 `y_true` 滞后成测量温度。
///
/// 传感器不与电芯同温，测量值是真实温度经 `1/(τ_s·s+1)` 后的结果。控制器只能看到测量
/// 值，所以仿真必须串这一级，否则「温度上升有延时」被低估、保护会显得比实际更快。
#[derive(Debug, Clone)]
pub struct Sensor {
    tau: f64,
    y: f64,
}

impl Sensor {
    /// 建传感器：时间常数 `tau`（s），初始测量值 `y0`（℃）。`tau ≤ 0` 表示理想无滞后。
    pub fn new(tau: f64, y0: f64) -> Self {
        Self { tau, y: y0 }
    }

    /// 当前测量温度（℃）。
    pub fn reading(&self) -> f64 {
        self.y
    }

    /// 真实温度 `y_true` 持续 `dt` 秒后的新测量值。理想传感器（τ≤0）直接透传。
    pub fn measure(&mut self, y_true: f64, dt: f64) -> f64 {
        if self.tau > 0.0 {
            // 精确离散 `1 − exp(−dt/τ)`，避免 `dt/τ` 前向 Euler 在 τ<dt 时过冲。
            self.y += (1.0 - (-dt / self.tau).exp()) * (y_true - self.y);
        } else {
            self.y = y_true;
        }
        self.y
    }
}

/// 纯滞后缓冲：延迟 `round(τ/dt)` 步。`Plant` 与 Smith 预估器共用。
#[derive(Debug, Clone)]
pub struct DelayLine {
    buf: Vec<f64>,
    head: usize,
}

impl DelayLine {
    /// 建一条长度 `round(τ/dt)` 的缓冲，初始填满 `u0`。
    pub fn new(tau: f64, dt: f64, u0: f64) -> Self {
        Self { buf: vec![u0; delay_len(tau, dt)], head: 0 }
    }

    /// 推入当前电流，返回延迟到期、此刻生效的电流。
    ///
    /// 缓冲长度在建线时按 `round(τ/dt)` 固定，因此这里**不接受** dt —— 传入 dt 也无法
    /// 改变行进而只会误导调用方（守护进程固定 1s 采样）。
    pub fn push(&mut self, u: f64) -> f64 {
        if self.buf.is_empty() {
            return u;
        }
        let old = self.buf[self.head];
        self.buf[self.head] = u;
        self.head = (self.head + 1) % self.buf.len();
        old
    }
}

/// 滞后步数 `round(τ/dt)`，至少 0。
fn delay_len(tau: f64, dt: f64) -> usize {
    if tau <= 0.0 || dt <= 0.0 {
        return 0;
    }
    (tau / dt).round().max(0.0) as usize
}

/// 阶跃实测中的一个采样点。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// 相对阶跃施加时刻的时间（s）。
    pub t: f64,
    /// 温度（℃）。
    pub y: f64,
}

impl From<(f64, f64)> for Sample {
    fn from((t, y): (f64, f64)) -> Self {
        Self { t, y }
    }
}

/// 辨识失败原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentifyError {
    /// 采样点少于 2 个。
    NotEnoughSamples,
    /// 阶跃幅度为 0。
    ZeroStep,
    /// 温度几乎没变化，无法辨识。
    NoResponse,
    /// 未能找到 28.3% / 63.2% 两个特征时刻。
    CrossingNotFound,
    /// 63.2% 时刻不晚于 28.3% 时刻，数据非单调或噪声过大。
    NonMonotonic,
    /// 增益为负：加大电流反而降温，与被控对象物理方向矛盾，视为坏数据。
    NegativeGain,
}

impl core::fmt::Display for IdentifyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Self::NotEnoughSamples => "采样点少于 2 个",
            Self::ZeroStep => "阶跃幅度为 0，无法辨识增益",
            Self::NoResponse => "温度无明显响应，无法辨识",
            Self::CrossingNotFound => "未找到 28.3%/63.2% 特征时刻",
            Self::NonMonotonic => "响应非单调（63.2% 不晚于 28.3%），数据质量不足",
            Self::NegativeGain => "增益为负：加大电流反而降温，数据方向矛盾",
        };
        f.write_str(s)
    }
}

impl std::error::Error for IdentifyError {}

/// 一段阶跃实验的数据。
///
/// 阶跃前稳态温度取自 `samples[0].y`，不另设字段，避免第二个真源。
#[derive(Debug, Clone, Copy)]
pub struct StepExperiment<'a> {
    /// 阶跃前电流（A）。
    pub u_before: f64,
    /// 阶跃后电流（A）。
    pub u_after: f64,
    /// 采样序列，`t=0` 为阶跃施加时刻；`samples[0]` 即阶跃前的温度。
    pub samples: &'a [Sample],
}

impl StepExperiment<'_> {
    /// 阶跃前稳态温度：采样序列首点。
    pub fn y_before(&self) -> f64 {
        self.samples[0].y
    }
}

/// 两点法辨识 FOPDT 的 K / T / τ。
///
/// 采用 28.3% 与 63.2% 两个特征点：
///
/// ```text
/// K = Δy / Δu
/// T = 1.5 · (t63.2 − t28.3)
/// τ = t63.2 − T
/// ```
///
/// 两个特征时刻由采样序列线性插值得到，因此采样步长不影响精度。
pub fn identify(exp: &StepExperiment<'_>) -> Result<PlantModel, IdentifyError> {
    if exp.samples.len() < 2 {
        return Err(IdentifyError::NotEnoughSamples);
    }
    let du = exp.u_after - exp.u_before;
    if du.abs() < 1e-12 {
        return Err(IdentifyError::ZeroStep);
    }
    let y_before = exp.y_before();
    let y_end = exp.samples[exp.samples.len() - 1].y;
    let dy = y_end - y_before;
    if dy.abs() < 1e-9 {
        return Err(IdentifyError::NoResponse);
    }

    let t1 = crossing_time(exp.samples, y_before, dy, 0.283).ok_or(IdentifyError::CrossingNotFound)?;
    let t2 = crossing_time(exp.samples, y_before, dy, 0.632).ok_or(IdentifyError::CrossingNotFound)?;
    if t2 <= t1 {
        return Err(IdentifyError::NonMonotonic);
    }

    let k = dy / du;
    if k <= 0.0 {
        return Err(IdentifyError::NegativeGain);
    }
    let t = 1.5 * (t2 - t1);
    let tau = (t2 - t).max(0.0);
    PlantModel::new(k, t, tau).ok_or(IdentifyError::NoResponse)
}

/// 求响应首次达到 `frac` 比例的时刻，线性插值。
fn crossing_time(samples: &[Sample], y_before: f64, dy: f64, frac: f64) -> Option<f64> {
    let target = y_before + frac * dy;
    let reached = |y: f64| if dy > 0.0 { y >= target } else { y <= target };
    for w in samples.windows(2) {
        let (a, b) = (w[0], w[1]);
        if !reached(a.y) && reached(b.y) {
            let span = b.y - a.y;
            if span.abs() < 1e-12 {
                return Some(b.t);
            }
            let r = (target - a.y) / span;
            return Some(a.t + r * (b.t - a.t));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simulate_step(model: PlantModel, ambient: f64, u0: f64, u1: f64, dt: f64, steps: usize) -> Vec<Sample> {
        let mut p = Plant::new(model, ambient, u0, dt);
        let mut out = Vec::with_capacity(steps + 1);
        out.push(Sample { t: 0.0, y: p.temp() });
        for i in 0..steps {
            p.step(u1, dt);
            out.push(Sample { t: (i + 1) as f64 * dt, y: p.temp() });
        }
        out
    }

    /// 验收①：三组任意 FOPDT 数据的反算误差均 < 5%。
    #[test]
    fn identify_recovers_known_models_within_5_percent() {
        let cases = [
            (PlantModel { k: 0.85, t: 120.0, tau: 10.0 }, 25.0, 0.0, 20.0),
            (PlantModel { k: 1.05, t: 180.0, tau: 25.0 }, 27.0, 4.0, 24.0),
            (PlantModel { k: 2.40, t: 45.0, tau: 3.0 }, 30.0, 2.0, 9.0),
        ];
        for (truth, ambient, u0, u1) in cases {
            let samples = simulate_step(truth, ambient, u0, u1, 1.0, 4000);
            let got = identify(&StepExperiment { u_before: u0, u_after: u1, samples: &samples })
                .expect("辨识应当成功");
            let rel = |a: f64, b: f64| (a - b).abs() / b;
            assert!(rel(got.k, truth.k) < 0.05, "K 误差过大: {got:?} vs {truth:?}");
            assert!(rel(got.t, truth.t) < 0.05, "T 误差过大: {got:?} vs {truth:?}");
            // τ 相对误差用 T 归一化：小 τ 场景下绝对秒数误差才有意义。
            assert!((got.tau - truth.tau).abs() / truth.t < 0.05, "τ 误差过大: {got:?} vs {truth:?}");
        }
    }

    #[test]
    fn identify_rejects_bad_input() {
        let one = [Sample { t: 0.0, y: 25.0 }];
        assert_eq!(
            identify(&StepExperiment { u_before: 0.0, u_after: 1.0, samples: &one }),
            Err(IdentifyError::NotEnoughSamples)
        );
        let flat = [Sample { t: 0.0, y: 25.0 }, Sample { t: 10.0, y: 25.0 }];
        assert_eq!(
            identify(&StepExperiment { u_before: 0.0, u_after: 0.0, samples: &flat }),
            Err(IdentifyError::ZeroStep)
        );
        assert_eq!(
            identify(&StepExperiment { u_before: 0.0, u_after: 1.0, samples: &flat }),
            Err(IdentifyError::NoResponse)
        );
        // 物理方向矛盾：加电流却降温，必须报错而非靠 abs() 洗白。
        let cooling =
            [Sample { t: 0.0, y: 40.0 }, Sample { t: 100.0, y: 30.0 }, Sample { t: 200.0, y: 25.0 }];
        assert_eq!(
            identify(&StepExperiment { u_before: 0.0, u_after: 10.0, samples: &cooling }),
            Err(IdentifyError::NegativeGain)
        );
    }

    /// 验收②：默认参数下 25℃ 起步、满流 24A 的开环升温复现到 47℃ 量级，
    /// 且 τ 秒之前温度一动不动（纯滞后）。
    #[test]
    fn default_params_reproduce_open_loop_warmup() {
        let p = Params::default();
        let mut plant = Plant::new(p.model, p.ambient, 0.0, 1.0);
        assert!((plant.temp() - 25.0).abs() < 1e-9, "静止起步应为环境温度");

        let dt = 1.0;
        let mut cross_47 = None;
        let mut trace = Vec::new();
        for i in 0..3600 {
            let y = plant.step(p.current_max, dt);
            trace.push(y);
            if cross_47.is_none() && y >= 47.0 {
                cross_47 = Some((i + 1) as f64 * dt);
            }
        }

        // 纯滞后：τ=25s，前 25 步温度必须严格等于起点。
        for (i, y) in trace.iter().take(25).enumerate() {
            assert!((y - 25.0).abs() < 1e-12, "第 {} 步不应响应：{y}", i + 1);
        }
        assert!(trace[25] > 25.0, "τ 之后必须开始响应");

        let t_cross = cross_47.expect("应穿越 47℃");
        assert!((300.0..=600.0).contains(&t_cross), "穿越 47℃ 时刻 {t_cross}s 不在 5~10 分钟量级");
        let y_inf = plant.temp();
        assert!((y_inf - 50.2).abs() < 0.1, "开环稳态 {y_inf}℃ 应高于设定点（说明必须闭环限流）");
        assert!(y_inf > p.hardware_limit, "开环稳态必须越过硬件保护温度，闭环才有意义");
    }

    /// 验收③：参数唯一真源——派生阈值随设定点联动，替换模型即改变仿真结果。
    #[test]
    fn params_are_single_source_of_truth() {
        let p = Params::default();
        // 设定点 47.0 → 介入 45.0、超温 47.3。与硬件线 48.0 各留 1℃ 与 0.7℃ 余量。
        assert!((p.intervention_temp() - 45.0).abs() < 1e-9);
        assert!((p.overtemp_temp() - 47.3).abs() < 1e-9);
        assert!((p.hardware_limit - 48.0).abs() < 1e-9);
        assert!((p.current_max - 24.0).abs() < 1e-9);

        let mut q = p;
        q.setpoint = 45.0;
        assert!((q.intervention_temp() - 43.0).abs() < 1e-9, "介入点必须跟随设定点");
        assert!((q.overtemp_temp() - 45.3).abs() < 1e-9, "超温点必须跟随设定点");

        let dt = 1.0;
        let mut a = Plant::new(p.model, p.ambient, 0.0, dt);
        let mut b = Plant::new(PlantModel { k: p.model.k * 2.0, ..p.model }, p.ambient, 0.0, dt);
        for _ in 0..600 {
            a.step(p.current_max, 1.0);
            b.step(p.current_max, 1.0);
        }
        assert!(b.temp() > a.temp() + 10.0, "模型参数必须真的驱动仿真");

        assert!(p.clamp_current(-5.0) == 0.0 && p.clamp_current(99.0) == 24.0);
    }

    /// 阈值不许在别处再写一遍：`src/` 下除本文件外不得出现旧实现的字面量。
    #[test]
    fn no_duplicate_threshold_literals_outside_model() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).expect("读取 src 目录") {
                let p = e.expect("目录项").path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let mut files = Vec::new();
        walk(std::path::Path::new("src"), &mut files);
        assert!(files.len() >= 2, "至少应有 src/lib.rs 与 src/model.rs");
        let banned = ["47.3", "44.7", "24000000", "0.3;"];
        for f in files {
            if f.file_name().is_some_and(|n| n == "model.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&f).expect("读取源码");
            for (i, line) in text.lines().enumerate() {
                for b in banned {
                    assert!(!line.contains(b), "{}:{} 出现应集中在 model.rs 的字面量 {b}", f.display(), i + 1);
                }
            }
        }
    }
}
