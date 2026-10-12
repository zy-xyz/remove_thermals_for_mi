# thermald — IMC-ADRC 充电温控（小米 / 红米 KernelSU 模块）

对小米/红米设备充电时把电池温度**闭环**压在设定点（默认 47℃）之下，而不是靠模糊 PID 事后补救。核心控制器 IMC-ADRC（IMC 设定点整形 + 线性自抗扰 + Smith 滞后补偿），全部零第三方 crate、静态交叉编译，产物是单个 aarch64 静态二进制，装到 KernelSU 后即可用 WebUI 实时调参。

## 目录

- [原理：IMC-ADRC](#原理imc-adrc)
- [与旧模糊 PID 的差异](#与旧模糊-pid-的差异)
- [参数调优指南](#参数调优指南)
- [安装（KernelSU 模块）](#安装kernelsu-模块)
- [CLI 用法](#cli-用法)
- [WebUI](#webui)
- [交叉编译与打包](#交叉编译与打包)
- [风险与免责声明](#风险与免责声明)
- [目录布局](#目录布局)

## 原理：IMC-ADRC

被控对象是充电电流 `u(A)` → 电池温度 `y(℃)`，标称模型是**一阶惯性 + 纯滞后**（FOPDT）：

```
G(s) = K · e^(-τs) / (T·s + 1)
时域: T·dΔ/dt = K·u(t-τ) - Δ,  Δ = y - ambient
```

控制器（见 `src/control.rs` 顶部模块注释，本 README 只做忠实转述）：

1. **ADRC 化**。把一阶对象改写为 `y' = b0·u + f`：`b0 = K/T` 是**输入增益标称值**，`f` 是总扰动（由扩张状态观测器 ESO 的 `z2` 估计）。`b0` 刻意**独立于** `K/T` 存（`CtrlParams::b0`），因为 ADRC 的鲁棒性正来自 `b0` 可与真实增益失配——失配量被 `z2` 吸收。若让 `b0` 跟随 `K/T`，鲁棒性就没了。
2. **IMC 设定点整形**。设定点过 `1/(λs+1)` 得 `r_filtered`。用精确离散系数 `1 − exp(−dt/λ)` 而非前向 Euler `dt/λ`：后者在 `λ < dt` 时会把阶跃放大成 `dt/λ` 倍（`λ=0.01, dt=1` 即放大 100 倍）而发散；精确式对任意 `λ > 0` 恒稳定，WebUI 又要让用户自由调 `λ`，这里必须稳。
3. **Smith 滞后补偿**。纯滞后 `τ` 不改变幅频、只加相位。若把含滞后测量 `y` 直接喂 ESO，闭环可用带宽被压到 `~1/τ` 以下（`τ=25s` 时调节时间必然超 120 步）。用标称无滞后模型 `predictor` 算「本该有的输出」，减去「预估器的滞后副本」，得到无状态 Smith 修正：
   ```
   y_corrected = y + (predictor − predictor_delayed)
   ```
   标称一致时 `predictor_delayed == y`，控制器看到不含滞后的输出；模型失配时残差由 ESO 的 `z2` 吸收。
4. **线性 ESO**。`β1 = 2ωo`、`β2 = ωo²`，`z1` 跟踪输出、`z2` 跟踪总扰动。**喂给 ESO 的 `b0·u` 必须是实际施加的电流（限幅之后）**，否则观测器按一个从未发生的控制量推算状态，`z2` 被系统性污染 → 积分风卷。
5. **控制律 + 限幅**。
   ```
   u0 = ωc · (r_filtered − z1)
   u  = (u0 − z2) / b0
   u  ← clamp(u, current_min, current_max)
   ```
6. **起步无扰**。构造时 `z2 = −b0·u0`（`u0` 是当前真实电流）而非 0：否则首步电流被算成 0，温度先下探再回升。实测起步下探由 0.345℃ 归零。

采样周期 1s（与旧守护进程 `sleep(1)` 一致）。ESO 与对象预估器都用前向 Euler，稳定要求 `ωo·dt < 2`；默认 `ωo ≈ 7.75/T ≈ 0.0431`（`T=180s`）下 `dt < 46s`，余量充足。

## 与旧模糊 PID 的差异

| | 旧 `mi_thermald.c` 模糊 PID | 新 IMC-ADRC |
|---|---|---|
| 建模 | 无模型，规则表 + 梯形隶属函数 | 一阶惯性 + 纯滞后，参数显式 |
| 扰动 | 靠 P/D 项硬顶 | 显式估计（ESO `z2`），可被观测器吸收 |
| 纯滞后 | 无补偿，闭环带宽被 `1/τ` 压死 | Smith 修正把滞后折出反馈通道 |
| 起步 | 有 0.345℃ 下探 | `z2=−b0·u0` 无扰切换，实测归零 |
| 参数 | `kp/ki/kd_base` + `kp/ki/kd_delta` + `e_range`/`ec_range` | `k/t/τ`（对象）+ `wo/wc/λ/b0`（控制）|
| 语言 | C，函数级 `static last_error` 多实例污染 | Rust，控制器全状态在结构体里 |
| 可复现 | 不能重放（`static`） | 纯函数式 `step()`，同样输入序列逐位可复现 |

旧的模糊 PID 只作为对照仪器保留在 `src/legacy_baseline.rs`（Rust 忠实移植），**不参与生产控制路径**。原 C 版放在 `legacy/mi_thermald.c`，标注废弃、不参与构建。

## 参数调优指南

| 参数 | 作用 | 调高的代价 | 调低的代价 |
|---|---|---|---|
| `setpoint` | 目标温度（℃） | 温度上限放宽、快充更快 | 更早介入限流、快充变慢 |
| `intervention_delta` | 介入温差的余量；温度 < `setpoint−delta` 时直接满流 | 更早介入 | 更接近设定点才介入 |
| `overtemp_delta` | 超温余量；温度 > `setpoint+delta` 进入软斜坡限流 | 更早介入软限流 | 允许短时冲高 |
| `hardware_limit` | 硬件保护参考线（℃）。**本模块不实施硬保护**，只把温度压在此线以下 | 只是参考 | 只是参考 |
| `current_min` / `current_max` | 电流上下限（A）。执行器唯一约束 | 上限越高，越容易冲高 | 上限越低，充电越慢 |
| `k` (`PlantModel.k`) | 稳态增益 (℃/A)。单位电流引起的稳态温升 | 让控制器以为加热更快 | 让控制器以为加热更慢 |
| `t` (`PlantModel.t`) | 热时间常数（s），达 63.2% 所需 | 控制器响应更快、更激进 | 更保守 |
| `tau` (`PlantModel.tau`) | 纯滞后（s），从施加电流到温度开始响应 | 影响 Smith 缓冲长度 | 同上 |
| `wo` (rad/s) | ESO 带宽。**必须满足 `ωo·dt < 2`** | 更快收敛、抗扰更强 | 更慢收敛 |
| `wc` (rad/s) | 控制器带宽 | 响应更快、易振荡 | 更平稳但更慢 |
| `λ` (s) | IMC 滤波器时间常数 | 阶跃更平滑 | 阶跃更快、更接近 bang-bang |
| `b0` (℃/(A·s)) | 输入增益标称值。**故意独立于 `K/T`**——失配由 `z2` 吸收 | 鲁棒性下降 | 控制器认为输入更弱 |
| `sensor_tau` | 温度传感器热耦合滞后（s） | 修正过度 | 修正不足 |
| `sample_period` | 采样周期（s，守护进程） | 更省电、控制更粗 | 更精细但耗电 |

**起步推荐**：默认值（`setpoint=47, intervention_delta=2.0, overtemp_delta=0.3, hardware_limit=48, current_max=24`）先跑一天，用 WebUI 看 `overshoot_pct` 与 `settle_steps`（`thermald sim` 输出），再微调 `λ`（0.5–2s）和 `wc`（`wo/10`~`wo/3`）。

## 安装（KernelSU 模块）

```sh
# 1. 打 zip
bash scripts/package.sh
ls -la build/thermald-v0.1.0.zip

# 2. 通过 KernelSU 管理器安装
adb push build/thermald-v0.1.0.zip /sdcard/Download/
adb shell su -c "mount -o remount,rw /data/adb"
# 或在 KernelSU 管理器 App 中点 zip 安装（推荐）

# 3. 重启，验证
adb shell "ls /data/adb/modules/thermald/"
adb shell "su -c 'thermald status'"
```

模块路径布局（zip 顶层就是模块根）：

```
module.prop
customize.sh
service.sh
post-fs-data.sh
uninstall.sh
sepolicy.rule
bin/thermald
webroot/index.html, app.js, style.css
```

## 在线更新

装好之后不用再手动推 zip：KernelSU 管理器会读 `module.prop` 的 `updateJson` 检查新版本，有新版本直接点更新即可。

- 元数据 `update.json`（`version`/`versionCode`/`zipUrl`/`changelog`）由 `scripts/package.sh` 生成，随 Release 作为 asset 上传。
- URL 走 `gh-proxy.com`：实测该设备**直连 `github.com` 不通**，只有代理可达。
- 升级安装会保留 `/data/adb/modules/thermald/data/`（`config.json` 调参与覆盖开关），无需重新调参。
- 注意：**已装的旧版（≤0.1.0）其 `module.prop` 里没有 `updateJson`，检查不到更新**，需手动装一次 0.1.1 及以后版本，此后才能收到在线更新提示。

## CLI 用法

`thermald` 二进制，所有子命令**输出单行 JSON** 到 stdout，错误中文写 stderr、退出码见契约 §7。

```sh
thermald status                        # 实时状态
thermald get setpoint                  # 取单个参数
thermald set setpoint=46.5 wc=0.03     # 改参数（越界拒绝，文件不变）
thermald set --reset                   # 恢复出厂
thermald defaults                      # 打印出厂默认（不落盘）
thermald sim --steps 600 --mismatch 1.5
thermald history --limit 100
thermald daemon                        # 前台跑主循环
thermald version
```

`status` 返回的 `state` 取值：`idle`（未充电）/ `monitoring`（充电但低于介入点，满流）/ `controlling`（闭环限流中）/ `stopped`（守护进程未运行）。守护进程未运行时**不报错**：`ok:true, running:false, state:"stopped"`。

## WebUI

`webroot/` 下纯静态 HTML/CSS/JS，**无 CDN、无外链、无构建**。通过 KernelSU 调 CLI；无 `ksu` 全局对象（普通浏览器）时进入演示模式：顶部显示「未运行在 KernelSU 环境（演示模式）」，用内置样例数据画图。

`ksu.exec` 走 root shell（`cwd=/`），裸命令不在 `PATH`（实测 `thermald` → exit 127），故面板用 `ksu.moduleInfo().moduleDir` 拼绝对路径调用；模块目录名未必等于 `module.prop` 的 `id`，因此不硬编码。

- 轮询周期 ≤2s（可暂停）
- 参数表单字段与契约 §3 表格一一对应（含单位、范围、中文标签）
- 保存 = 调 `thermald set k=v`；重置 = `thermald set --reset`
- 保护状态指示：显示 `temp` 与 `hardware_limit` 的余量（**本模块不实施硬保护**）

打包时 webroot 会被 `scripts/package.sh` 复制进 zip 顶层的 `webroot/`，KernelSU 会把它复制到 `/data/adb/modules/thermald/webroot/`，WebUI 通过 KernelSU 内置 HTTP 桥访问。

## 交叉编译与打包

**版本单点**：以 `module/module.prop` 的 `version=` 为准，`Cargo.toml` 的 `version` 由 `scripts/build.sh` 断言一致（契约 §11）。

```sh
# 交叉编译 → build/thermald
bash scripts/build.sh
file build/thermald          # ELF 64-bit ... machine: aarch64, statically linked
sha256sum -c build/thermald.sha256

# 打模块 zip → build/thermald-v<version>.zip
bash scripts/package.sh
```

CI：`.github/workflows/build.yml` 在 `ubuntu-latest` 上装 Rust stable + `aarch64-unknown-linux-musl` target，跑 `cargo test --lib`（本机 25 项），再依次 `scripts/build.sh` → `scripts/package.sh`，并断言产物为真静态 ELF。推 tag `v*` 时通过 `softprops/action-gh-release@v2` 发 GitHub Release 附 zip。

### 交叉工具链说明

链接器用 **rustc 自带的 `rust-lld`**（`scripts/build.sh` 自动从 `rustc --print sysroot` 定位），不用 `zig cc`：

- zig 0.13 的 LLD 解析不了新版 rustc 产出的 `libcompiler_builtins-*.rlib`（报 `unknown cpu architecture: 65794`）；
- zig 会把缓存写进系统临时目录，受限沙箱（如 DSH workspace-write）下直接 `AccessDenied`。

rust-lld 与当前 rustc 同源，能正确解析这些 rlib。musl std 自带 self-contained 的 `crt1.o`/`crti.o`/`libc.a`/`libunwind.a`，所以**不需要外部 sysroot**。macOS 上 rust-lld 动态链接 `libLLVM.dylib`，脚本会设好 `DYLD_FALLBACK_LIBRARY_PATH`。

本仓库**完全不需要 zig**：无 C 源（`legacy/mi_thermald.c` 仅存档参考，不进构建），strip 由 rustc 的 `-C strip=symbols` 完成。

实测（macOS aarch64，rust stable）：

```
$ bash scripts/build.sh
── 产物 ─────────────────────────────────────
路径  : build/thermald
体积  : 439792 bytes
sha256: <每次构建不同，见 build/thermald.sha256>
目标  : aarch64-unknown-linux-musl（静态）
──────────────────────────────────────────────
build/thermald: ELF 64-bit LSB executable, ARM aarch64, version 1 (SYSV), statically linked, stripped
```

产物校验：`file build/thermald` 应显示 `ELF 64-bit LSB executable, ARM aarch64`；`readelf -l` 应**不含** `PT_INTERP`/`PT_DYNAMIC`（真静态）。当前产物已按此核对（aarch64 `e_type=EXEC`，segment 仅 `PHDR/LOAD/TLS/GNU_RELRO/GNU_EH_FRAME/GNU_STACK`）。

## 风险与免责声明

- **本模块不实施硬过温保护**。`hardware_limit`（默认 48℃）是**参考线**，不是断流开关。硬件过温保护由充电驱动（`thermal_engine` / `charge_early_halt`）负责；本控制器只负责用闭环（设定点 47℃、上限 48℃ 留 1℃ 余量）把温度压在此线以下。
- **模型失配风险**。参数 `k/t/τ` 是标称值，不同电池/散热片/环境差异会带来失配。`src/legacy_baseline.rs` 提供旧模糊 PID 作为对照仪器，`src/control.rs` 的 25 项单元测试覆盖 50%/100% `τ` 失配场景。真机首次安装建议先用 `THERMALD_DRY_RUN=1` 或 `thermald sim --mismatch 1.5` 观察。
- **纯滞后风险**。若标称 `tau` 与实际偏离过大，Smith 修正会失效，控制器退化到 IMC+ESO 但仍可用；不建议标称 `tau` 超过 60s。
- **限幅是唯一的执行器约束**。若真机充电驱动拒绝接受 >18A（部分机型），需相应下调 `current_max`，否则观测器会看到 `applied_current < u_raw` 而产生 `z2` 残差。
- **KernelSU 权限**。`sepolicy.rule` 允许读 `/sys/class/power_supply/battery/{temp,status,constant_charge_current}` 与写 `/data/adb/modules/thermald/data/`。请勿在其它 KernelSU 分支复用而未复核 selinux 规则。
- **免责**：本模块仅供技术爱好者在自有设备上试验。因使用本模块导致的高温、鼓包、起火、数据丢失，作者与分发者不承担任何责任。

## 目录布局

```
remove_thermals_for_mi/
├── Cargo.toml                 # 零第三方 crate
├── src/
│   ├── lib.rs
│   ├── model.rs               # FOPDT 参数唯一真源
│   ├── control.rs             # IMC-ADRC 控制律（本文原理章节所依据）
│   ├── legacy_baseline.rs     # 旧模糊 PID 的 Rust 移植（对照仪器）
│   └── main.rs                # CLI bin 入口
├── module/                    # KernelSU 模块目录
│   ├── module.prop            # version 单点
│   ├── bin/thermald           # 打包时从 build/thermald 复制
│   └── ...
├── webroot/                   # C 组：纯静态 WebUI
├── scripts/
│   ├── build.sh               # 交叉编译（本文交叉编译章节所依据）
│   ├── package.sh             # 打模块 zip
│   └── selftest-webui.sh
├── legacy/
│   └── mi_thermald.c          # 旧 C 版，已废弃，不参与构建
├── .github/workflows/build.yml
├── CONTRACT.md                # 跨组冻结契约（唯一真源）
└── README.md
```
