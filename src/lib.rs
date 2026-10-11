//! 小米/红米充电温控守护进程（KernelSU 模块）。
//!
//! 控制对象：充电电流 u(A) → 电池温度 y(℃)。
//! 算法：IMC-ADRC，见 [`control`]；标称对象模型见 [`model`]。
//! 旧方案（被替换的模糊 PID）仅作对照仪器，见 [`legacy_baseline`]。
//!
//! 运行侧：配置见 [`config`]，设备 I/O 见 [`device`]，状态落盘见 [`state`]，
//! 主循环见 [`daemon`]。跨组接口契约见仓库根 `CONTRACT.md`。

pub mod config;
pub mod control;
pub mod daemon;
pub mod device;
pub mod legacy_baseline;
pub mod model;
pub mod state;
