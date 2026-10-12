# 接口契约（冻结版）

> 本文是**跨组并行开发的唯一真源**。A/B/C/D 四组各自独立写文件，只通过本文约定交互。
> 改动本文需 Lead 同意；任何一方不得单方面改契约。

## 0. 硬约束（违反即返工）

1. **零第三方 crate**。`Cargo.toml` 的 `[dependencies]` 必须保持为空——本机离线，且目标机要全静态。
   JSON 手写、CLI 手写、无 clap/serde/anyhow。只用 `std` + 直接 extern libc 符号。
2. **不删改既有算法代码**：`src/model.rs`、`src/control.rs`、`src/legacy_baseline.rs` 的公共 API 已冻结并被 25 个测试锁定。
   需要新能力时**新增**文件与函数，不改这三个文件的签名。
3. 所有脚本 **LF 换行**、`#!/system/bin/sh` 或 `#!/usr/bin/env bash`、`set -e`。
4. 产物路径一律相对仓库根；不写绝对路径进仓库文件。

## 1. 目录布局（冻结）

```
remove_thermals_for_mi/
├── Cargo.toml                 # A 组
├── src/                       # A 组（model.rs/control.rs/legacy_baseline.rs 冻结只读）
│   ├── lib.rs
│   ├── model.rs               # 冻结
│   ├── control.rs             # 冻结
│   ├── legacy_baseline.rs     # 冻结
│   ├── config.rs              # A 新增
│   ├── device.rs              # A 新增
│   ├── state.rs               # A 新增
│   ├── daemon.rs              # A 新增
│   └── main.rs                # A 新增（CLI 入口，bin）
├── module/                    # B 组
│   ├── module.prop
│   ├── customize.sh
│   ├── service.sh
│   ├── post-fs-data.sh
│   ├── uninstall.sh
│   ├── sepolicy.rule
│   ├── bin/                   # 交叉编译产物落点（.gitignore，构建时填充）
│   └── webroot/               # C 组产出，B 只读引用（打包时复制）
├── webroot/                   # C 组（源目录）
│   ├── index.html
│   ├── app.js
│   └── style.css
├── scripts/
│   ├── build.sh               # D 组：交叉编译出静态二进制
│   ├── package.sh             # B 组：打模块 zip（内部调用 scripts/build.sh）
│   └── selftest-webui.sh      # C 组：webroot 自检
├── .github/workflows/build.yml# D 组
├── tests/                     # A 组集成测试
├── legacy/mi_thermald.c       # D 组：移入并注明废弃
└── README.md                  # D 组
```

## 2. 设备路径（冻结）

| 用途 | 路径 |
|---|---|
| 电池温度（0.1℃，整数） | `/sys/class/power_supply/battery/temp` |
| 充电状态（字符串） | `/sys/class/power_supply/battery/status` |
| 充电电流（µA，整数） | `/sys/class/power_supply/battery/constant_charge_current` |
| 模块数据目录 | `/data/adb/modules/thermald/data` |
| 配置文件 | `/data/adb/modules/thermald/data/config.json` |
| 状态快照 | `/data/adb/modules/thermald/data/state.json` |
| 历史 CSV | `/data/adb/modules/thermald/data/history.csv` |
| 运行日志 | `/data/adb/modules/thermald/data/thermald.log` |
| 单实例锁 | `/data/adb/modules/thermald/data/thermald.lock` |

数据目录位于模块目录内，**随模块卸载一并清除**。升级安装时由 `customize.sh`
从上一版目录抢救 `config.json` 与 `override_mi_thermald`（KernelSU 到下次开机才删旧目录）。

数据目录路径可被环境变量 `THERMALD_DIR` 覆盖（**测试用，必须支持**）。

`status` 读充电状态字符串的取值语义（大小写不敏感）：
`Charging`/`Full` → 充电中，参与控制；其余（`Discharging`/`Not charging`/`Unknown` 等）→ 非充电，电流归零并复位控制器。

## 3. 配置文件 JSON（冻结）

`config.json` 是**唯一可写参数真源**，由 `set` 子命令写、守护进程热加载、WebUI 读写。

```json
{
  "version": 1,
  "setpoint": 47.0,
  "intervention_delta": 1.0,
  "overtemp_delta": 0.3,
  "hardware_limit": 48.0,
  "current_min": 0.0,
  "current_max": 24.0,
  "ambient": 39.0,
  "sensor_tau": 8.0,
  "k": 1.05,
  "t": 180.0,
  "tau": 25.0,
  "wo": 0.04305555555555556,
  "wc": 0.04305555555555556,
  "lambda": 1.0,
  "b0": 0.005833333333333334,
  "csv_max_points": 600,
  "sample_period": 1.0
}
```

字段名与 `Params`/`CtrlParams` 的映射（A 组实现，其余组按名调用）：

| JSON 键 | Rust 字段 | 单位 | 合法范围 | WebUI 中文标签 |
|---|---|---|---|---|
| `setpoint` | `Params::setpoint` | ℃ | 30–47.5 | 目标温度 |
| `intervention_delta` | `Params::intervention_delta` | ℃ | 0–10 | 介入温度差（设定点 − 该值 = 开始限流） |
| `overtemp_delta` | `Params::overtemp_delta` | ℃ | 0–5 | 超温余量 |
| `hardware_limit` | `Params::hardware_limit` | ℃ | 40–60 | 硬件保护温度（只读参考值，本模块不实施保护） |
| `current_min` | `Params::current_min` | A | 0–5 | 电流下限 |
| `current_max` | `Params::current_max` | A | 1–30 | 电流上限 |
| `ambient` | `Params::ambient` | ℃ | 0–45 | 环境温度 |
| `sensor_tau` | `Params::sensor_tau` | s | 0–60 | 传感器热耦合滞后 |
| `k` | `PlantModel::k` | ℃/A | 0.01–10 | 稳态增益 |
| `t` | `PlantModel::t` | s | 10–2000 | 热时间常数 |
| `tau` | `PlantModel::tau` | s | 0–300 | 纯滞后 |
| `wo` | `CtrlParams::wo` | rad/s | 0.0001–1 | 观测器带宽 |
| `wc` | `CtrlParams::wc` | rad/s | 0.0001–1 | 控制器带宽 |
| `lambda` | `CtrlParams::lambda` | s | 0.05–60 | IMC 滤波时间常数 |
| `b0` | `CtrlParams::b0` | ℃/(A·s) | 1e-6–1 | 输入增益标称值 |
| `csv_max_points` | — | 点 | 60–10000 | 历史保留点数 |
| `sample_period` | — | s | 1–10 | 采样周期 |

**派生量（不落盘，由上式算出，禁止单独存储）**：
`intervention_temp = setpoint − intervention_delta`；`overtemp_temp = setpoint + overtemp_delta`。

**校验规则**：任一字段越界、非数字、缺项 → `set` 拒绝并**不写文件**（保持原文件逐字节不变），退出码非 0，stderr 打印中文原因。
未知键 → 拒绝（防拼写错误静默丢失）。

## 4. CLI 契约（冻结）——C 组按此写 WebUI，A 组实现

二进制名 `thermald`。所有子命令**输出单行 JSON** 到 stdout 并换行；错误时 stderr 中文 + 退出码非 0。
所有 JSON 均为 UTF-8、无 BOM、不带尾随逗号、数字用 JSON 合法表示（**不得输出 `NaN`/`inf`**，哨兵用 `null`）。

### 4.1 `thermald status`
```json
{
  "ok": true,
  "version": "0.1.0",
  "running": true,
  "pid": 12345,
  "uptime_s": 3600,
  "ts": 1791700000,
  "temp": 46.83,
  "current": 20.76,
  "target": 47.0,
  "intervention_temp": 45.0,
  "overtemp_temp": 47.3,
  "setpoint": 47.0,
  "hardware_limit": 48.0,
  "charging": true,
  "state": "controlling",
  "observer_z1": 46.80,
  "observer_z2": -0.1205,
  "filtered_setpoint": 47.0,
  "applied_current": 20.76,
  "actual_current": 19.52,
  "config": { "...同 config.json 全字段..." },
  "history_points": 512
}
```
- `state` 取值：`idle`（未充电）/`monitoring`（充电但低于介入点，满流）/`controlling`（闭环限流中）/`stopped`（守护进程未运行）。
- 守护进程未运行时**不得报错**：返回 `ok:true, running:false, state:"stopped"`，其余字段用配置文件默认值填充，`temp`/`current` 为 `null`。

### 4.2 `thermald get <key>`
```json
{"ok": true, "key": "setpoint", "value": 47.0}
```
未知 key → `{"ok": false, "error": "未知参数: xxx"}`，退出码 1。

### 4.3 `thermald set k1=v1 k2=v2 ...`
```json
{"ok": true, "changed": {"setpoint": 46.5}, "config": { "...全字段..." }}
```
非法值 → `{"ok": false, "error": "setpoint 超出范围 [30, 47.5]: 99"}`, 退出码 1，文件不变。

### 4.4 `thermald defaults`
```json
{"ok": true, "config": { "...出厂默认全字段..." }}
```
（打印但不落盘；落盘由 `set --reset` 或 `set` 完成）

### 4.5 `thermald set --reset`
把配置恢复出厂并落盘：
```json
{"ok": true, "changed": { "...所有与出厂不同的键..." }, "config": { "..." }}
```

### 4.6 `thermald sim [--setpoint 47.0] [--steps 600] [--mismatch 1.5]`
跑一次闭环仿真（用当前配置 + 覆盖项），输出指标：
```json
{
  "ok": true,
  "steps": 600,
  "overshoot_pct": 0.0,
  "settle_steps": 106,
  "steady_error": 0.0004,
  "peak_temp": 47.0183,
  "final_temp": 47.0,
  "current_min_seen": 0.0,
  "current_max_seen": 24.0,
  "hardware_limit": 48.0,
  "limit_respected": true,
  "mismatch": 1.0
}
```
`settle_steps` 用「首次入带 ±0.1℃」语义（见 `control.rs` 场景③注释里的证据）。
`limit_respected` = 全程 `peak_temp < hardware_limit`。

### 4.7 `thermald history [--limit N]`
```json
{"ok": true, "count": 512, "points": [{"t": 1791700000, "temp": 46.83, "current": 20.76, "target": 47.0, "state": "controlling"}, ...]}
```
默认返回全部（≤ `csv_max_points`）。

### 4.8 其它
- `thermald daemon`：前台跑主循环（供 service.sh 用 `nohup` 拉起）。
- `thermald version`：`{"ok": true, "version": "0.1.0"}`。
- 无参数/未知子命令：打印用法到 stderr，退出码 2。

### 4.9 实测锚定屋顶（A 组实现约束）

写入 `constant_charge_current` 的是电流**上限**：上限高于电池实吸电流时该写入不产生后果
（真机设定 24A、实吸 ~14A）。因此实际写入值不是裸的 ADRC 输出，而是：

```
u_written = min(u_adrc, roof)
```

`roof` 由 `control.rs::Anchor` 给出：温度 ≤ `intervention_temp` 时 `+∞`（不锚定）；超过
介入点后以「进入锚定瞬间的实测电流 `u_ref`」为基准，随温度线性收紧，到 `hardware_limit`
处归零。`u_ref` **捕获后冻结**，不得改用实时实测值——否则锚定线随实吸电流逐拍回落而自我
塌陷。解除带 `ANCHOR_RELEASE_HYST`（默认 0.3℃，字面量集中在 `model.rs`）。

## 5. state.json（冻结）

守护进程每 `sample_period` 秒原子写（写临时文件 + rename）：

```json
{
  "ts": 1791700000,
  "uptime_s": 3600,
  "pid": 12345,
  "state": "controlling",
  "temp": 46.83,
  "current": 20.76,
  "target": 47.0,
  "applied_current": 20.76,
  "actual_current": 19.52,
  "observer_z1": 46.80,
  "observer_z2": -0.1205,
  "filtered_setpoint": 47.0,
  "charging": true
}
```
- `applied_current`：控制器**命令**写进 `constant_charge_current` 的设定电流（A）；
  `actual_current`：电池侧**实测**电流（A，即 `current_now`）。
  两者通常不等：CV 阶段实际电流低于设定值。守护进程把实测电流喂进 ESO/预估器
  （`step_with_meas`），避免观测器按从未发生的输入推算状态。
- 读不到 `actual_current` 时为 `null`；旧版快照无此字段按 `null` 处理，**不视为损坏**。
`status` 的字段名与此一致（`status` 是 `state.json` + 配置 + 行情的超集）。

## 6. history.csv（冻结）

```csv
t,temp,current,target,actual,state
1791700000,46.83,20.76,47.0,19.52,controlling
```
- 表头固定，无空格。
- 环形：行数（不含表头）超过 `csv_max_points` 时从**头部**淘汰。
- `t` 单调递增。
- `temp`/`current`/`actual` 保留 3 位小数，`target` 1 位。
- `actual` 读不到时写空字段（列位置保留）；旧版 5 列 CSV 仍可读入，`actual` 按缺失处理。

## 7. 退出码（冻结）

| 码 | 含义 |
|---|---|
| 0 | 成功 |
| 1 | 参数/配置错误（含非法 set、未知 key） |
| 2 | 用法错误（未知子命令、缺参数） |
| 3 | 环境错误（无锁权限、目录不可写） |

第二个实例启动时：**退出码 0**，日志说明已有实例，不留孤儿进程。

## 8. sysfs 抽象（A 组内部，B/C 只需知道 dry-run 开关）

环境变量 `THERMALD_DRY_RUN=1` + `THERMALD_SCRIPT=<文件>` 启用假设备：
脚本文件每行一步，格式 `temp_c,status,current_a`，如 `40.0,Charging,24.0`。
dry-run 下**绝不触碰任何 `/sys` 路径**。写电流只记录到内存，可被 `status` 的 `applied_current` 反映。

## 9. 打包契约（B 组实现 `scripts/package.sh`，D 组 CI 调用）

- 输入：`build/thermald`（aarch64 静态二进制，由 `scripts/build.sh` 产出）
- 输出：`build/thermald-v<version>.zip`
- 附加输出：`build/update.json`——KernelSU 在线更新元数据（字段 `version`/`versionCode`/`zipUrl`/`changelog`，对齐 KSU `ModuleRepositoryImpl.checkUpdate`）。作为 Release asset 与 zip 一同上传，**不进模块 zip**。
- zip 顶层**直接**是 `module.prop`/`customize.sh`/`service.sh`/`post-fs-data.sh`/`uninstall.sh`/`sepolicy.rule`/`bin/thermald`/`webroot/*`，**无外层目录**。
- `version` 从 `module/ module.prop` 单点读取；`Cargo.toml` 的 version 由 `scripts/build.sh` 校验一致（不一致则报错退出）。
- `bin/thermald` 在 zip 内保留 0755。

## 10. WebUI 契约（C 组）

- 纯静态：`webroot/index.html` + `app.js` + `style.css`，**无 CDN、无 http(s) 外链、无构建步骤**。
- 通过 `ksu.exec("thermald status")` 等调用 CLI；**无 `ksu` 全局对象时**（普通浏览器）进入演示模式：页面正常渲染、顶部显示「未运行在 KernelSU 环境（演示模式）」，用内置样例数据画图，不抛未捕获异常。
- 轮询周期 ≤2s（可暂停）。
- 参数表单字段与 §3 表格一一对应（含单位、范围、中文标签）。
- 保存 = 调 `thermald set k=v`；重置 = `thermald set --reset`。
- 保护状态指示：显示 `temp` 与 `hardware_limit` 的关系（本模块不实施硬保护，只显示余量）。

## 11. 版本单点（D 组）

`module/module.prop` 的 `version=` 与 `Cargo.toml` 的 `version` 必须一致，由 `scripts/build.sh` 断言。
`versionCode` = `major*10000 + minor*100 + patch`。
