//! 设备层：sysfs 读写 + 干跑（dry-run）假设备。
//!
//! 所有 sysfs 路径集中在此。干跑模式（`THERMALD_DRY_RUN=1`）下**绝不触碰 `/sys`**，
//! 温度按 `THERMALD_SCRIPT` 指定的脚本文件推进，电流写入只记在内存里。
//! 这样 CI 与开发机能完整跑守护进程与 CLI，而设备侧行为一致。

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

/// sysfs 路径（可在测试中重定向，见 [`Device::with_root`]）。
#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self { root: PathBuf::from("/sys/class/power_supply/battery") }
    }
}

impl Paths {
    pub fn temp(&self) -> PathBuf {
        self.root.join("temp")
    }
    pub fn status(&self) -> PathBuf {
        self.root.join("status")
    }
    pub fn current(&self) -> PathBuf {
        self.root.join("constant_charge_current")
    }
    /// 电池真实电流节点（只读，µA）。
    pub fn current_now(&self) -> PathBuf {
        self.root.join("current_now")
    }
}

/// 充电状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charging {
    Yes,
    No,
}

/// 设备抽象：真实 sysfs 或干跑脚本。
pub struct Device {
    paths: Paths,
    dry: bool,
    /// 干跑脚本的每一步 `(temp_c, status, current_a)`。
    script: Vec<(f64, Charging, f64)>,
    /// 干跑脚本游标。
    cursor: RefCell<usize>,
    /// 干跑模式下最近一次写入的电流（真实模式为 `None`）。
    last_write: RefCell<Option<f64>>,
}

impl Device {
    /// 按环境变量决定真实设备还是干跑。
    pub fn from_env() -> Self {
        Self::from_env_with_root(Paths::default())
    }

    /// 同上，但可指定 sysfs 根（测试用）。
    pub fn from_env_with_root(paths: Paths) -> Self {
        let dry = std::env::var("THERMALD_DRY_RUN").map(|v| v == "1").unwrap_or(false);
        let script = if dry {
            let file = std::env::var("THERMALD_SCRIPT").unwrap_or_default();
            Self::load_script(&file)
        } else {
            Vec::new()
        };
        Self { paths, dry, script, cursor: RefCell::new(0), last_write: RefCell::new(None) }
    }

    /// 构造一个干跑设备（测试用，不经环境变量）。
    pub fn dry_with_script(script: Vec<(f64, Charging, f64)>) -> Self {
        Self {
            paths: Paths::default(),
            dry: true,
            script,
            cursor: RefCell::new(0),
            last_write: RefCell::new(None),
        }
    }

    /// 脚本格式：每行 `temp_c,status,current_a`。空行与 `#` 注释忽略。
    fn load_script(file: &str) -> Vec<(f64, Charging, f64)> {
        let Ok(text) = fs::read_to_string(file) else {
            return Vec::new();
        };
        text.lines()
            .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
            .filter_map(|l| {
                let mut it = l.split(',');
                let t: f64 = it.next()?.trim().parse().ok()?;
                let s = it.next()?.trim().to_string();
                let c: f64 = it.next()?.trim().parse().ok()?;
                Some((t, parse_status(&s), c))
            })
            .collect()
    }

    pub fn is_dry(&self) -> bool {
        self.dry
    }

    /// 读测量温度（℃）。真实模式读 sysfs 的 0.1℃ 整数。
    pub fn read_temp(&self) -> Option<f64> {
        if self.dry {
            let i = *self.cursor.borrow();
            if self.script.is_empty() {
                return None;
            }
            // 走到末尾后停在最后一步（设备保持终态）。
            let idx = i.min(self.script.len() - 1);
            return Some(self.script[idx].0);
        }
        let raw = fs::read_to_string(self.paths.temp()).ok()?;
        raw.trim().parse::<f64>().ok().map(|v| v / 10.0)
    }

    /// 读充电状态。读不到时返回 `None`（调用方按非充电处理，宁可断流不越温）。
    pub fn read_charging(&self) -> Option<Charging> {
        if self.dry {
            let i = *self.cursor.borrow();
            if self.script.is_empty() {
                return None;
            }
            return Some(self.script[i.min(self.script.len() - 1)].1);
        }
        let raw = fs::read_to_string(self.paths.status()).ok()?;
        Some(parse_status(raw.trim()))
    }

    /// 读电池真实电流（A）。节点是无符号 µA：放电时也报正值，故取绝对值。
    /// 读不到返回 `None`（调用方显示 `--`，不影响控制）。
    pub fn read_current_now(&self) -> Option<f64> {
        if self.dry {
            let i = *self.cursor.borrow();
            if self.script.is_empty() {
                return None;
            }
            return Some(self.script[i.min(self.script.len() - 1)].2);
        }
        let raw = fs::read_to_string(self.paths.current_now()).ok()?;
        raw.trim().parse::<f64>().ok().map(|ua| (ua / 1_000_000.0).abs())
    }

    /// 写充电电流（A）。真实模式转成 µA 整数写 sysfs。
    pub fn write_current(&self, amps: f64) -> Result<(), String> {
        if self.dry {
            *self.last_write.borrow_mut() = Some(amps);
            return Ok(());
        }
        let ua = (amps * 1_000_000.0).round() as i64;
        let path = self.paths.current();
        // sysfs 属性默认 0444（且内核会复位），写前必须放开权限，否则必然 EACCES。
        // 实测：Enforcing 与 Permissive 下 0444 均失败、0777 均成功 → 与 SELinux 无关。
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o777));
        }
        fs::write(&path, format!("{ua}\n"))
            .map_err(|e| format!("写入电流失败 {}: {e}", path.display()))
    }

    /// 干跑模式下最近一次写入的电流。
    pub fn last_written(&self) -> Option<f64> {
        *self.last_write.borrow()
    }

    /// 干跑模式推进脚本一步。返回是否真的前进了（已在末尾则 `false`）。
    pub fn advance(&self) -> bool {
        let mut c = self.cursor.borrow_mut();
        if *c + 1 < self.script.len() {
            *c += 1;
            true
        } else {
            false
        }
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// 判断路径是否存在（用于 CLI 提示设备是否可用）。
    pub fn exists(&self, p: &Path) -> bool {
        p.exists()
    }
}

/// "Charging"/"Full" → Yes；其余（含 Unknown/Discharging/Not charging）→ No。
pub fn parse_status(s: &str) -> Charging {
    match s.trim().to_ascii_lowercase().as_str() {
        "charging" | "full" => Charging::Yes,
        _ => Charging::No,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// 回归：sysfs 属性默认 0444，真实写入前必须先放开权限，否则必然 EACCES。
    /// 曾漏掉这一步，设备日志出现 757 次 `写入电流失败 ... Permission denied`。
    ///
    /// 用临时目录模拟 sysfs 节点，不经 `THERMALD_DRY_RUN`，走真实 `fs::write` 分支。
    #[test]
    fn write_current_chmods_readonly_node() {
        let dir = std::env::temp_dir().join(format!("thermald-wt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let node = dir.join("constant_charge_current");
        fs::write(&node, "0\n").unwrap();
        // 复刻设备上的 0444：属主也不可写。
        fs::set_permissions(&node, fs::Permissions::from_mode(0o444)).unwrap();
        assert_eq!(fs::metadata(&node).unwrap().permissions().mode() & 0o200, 0);

        std::env::remove_var("THERMALD_DRY_RUN");
        let dev = Device::from_env_with_root(Paths { root: dir.clone() });
        assert!(!dev.is_dry(), "不得走干跑分支，否则测不到真实写入");

        dev.write_current(3.0).expect("0444 节点写入应成功（内部先 chmod 0777）");
        assert_eq!(fs::read_to_string(&node).unwrap().trim(), "3000000");

        let _ = fs::remove_dir_all(&dir);
    }
}
