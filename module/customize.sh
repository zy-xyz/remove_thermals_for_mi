#!/system/bin/sh
# KernelSU 安装钩子（由 update-binary source 执行，运行在 BusyBox ash 独立模式）
# 职责：架构校验 → 打印信息 → 修权限 → 建数据目录 → 落地默认配置
set -e

# ── 可测性兜底：脱离 KernelSU 环境（本机自测）时提供 ui_print/abort/set_perm ──
# KernelSU 会把这三个函数导入当前 shell；本机没有，故仅在缺失时定义。
command -v ui_print >/dev/null 2>&1 || ui_print() { echo "$@"; }
command -v abort >/dev/null 2>&1 || abort() { echo "$@" >&2; exit 1; }
command -v set_perm >/dev/null 2>&1 || set_perm() { chown "$2:$3" "$1" 2>/dev/null || true; chmod "$4" "$1"; }
command -v set_perm_recursive >/dev/null 2>&1 || set_perm_recursive() {
    chown -R "$2:$3" "$1" 2>/dev/null || true
    find "$1" -type d -exec chmod "$4" {} + 2>/dev/null || true
    find "$1" -type f -exec chmod "$5" {} + 2>/dev/null || true
}

# 本脚本由 KernelSU 的 installer.sh **source** 执行（`. $MODPATH/customize.sh`），
# 故 $0 指向外层脚本、不是本文件，`${0%/*}` 会取到错误路径。
# MODPATH 由 installer.sh 以全局变量导出（= 模块实际安装目录），优先用它；
# 脱离 KernelSU 环境（本机自测）时退回 ${0%/*}。
MODDIR=${MODPATH:-${0%/*}}
# 数据目录：模块目录下 data/，随模块走（卸载即清）。契约 §2 允许 THERMALD_DIR 覆盖（测试用）。
# 安装期 MODDIR = /data/adb/modules_update/thermald，开机时被 rename 成
# /data/adb/modules/thermald，故 $MODDIR/data 即最终数据目录。
DATA_DIR=${THERMALD_DIR:-$MODDIR/data}

# ── 架构校验：本模块只交叉编译了 aarch64 静态二进制 ──
if [ "$ARCH" != "arm64" ]; then
    abort "本模块仅支持 arm64 设备，当前架构: $ARCH"
fi

ui_print "**************************************"
ui_print " thermald 充电温控"
ui_print " 版本: $(sed -n 's/^version=//p' "$MODDIR/module.prop" | head -1)"
ui_print " 架构: $ARCH   API: $API"
ui_print "**************************************"
ui_print "- 安装模块文件"

[ -f "$MODDIR/bin/thermald" ] || abort "安装包损坏：缺少 bin/thermald"

# 0755：可执行（守护进程由 service.sh 拉起，不经 /system）
set_perm "$MODDIR/bin/thermald" 0 0 0755
set_perm "$MODDIR/service.sh" 0 0 0755
set_perm "$MODDIR/post-fs-data.sh" 0 0 0755
set_perm "$MODDIR/uninstall.sh" 0 0 0755
# WebUI 静态资源：只读（C 组未产出时目录不存在，跳过）
if [ -d "$MODDIR/webroot" ]; then
    set_perm_recursive "$MODDIR/webroot" 0 0 0755 0644
fi

# ── 数据目录：状态/配置/日志/CSV/锁文件都住在这里 ──
ui_print "- 创建数据目录 $DATA_DIR"
mkdir -p "$DATA_DIR"
chmod 0700 "$DATA_DIR"

# ── 升级安装：抢救上一版数据 ──
# KernelSU 到下次开机才 rm -rf 旧模块目录，此刻旧目录仍在，故在此把用户
# 调过的配置与覆盖开关搬进新模块目录，避免每次升级丢参数。
# 两个历史来源都要看：
#   0.1.0 及更早：固定路径 /data/adb/thermald
#   0.1.1+     ：模块目录下 data/
# 注意模块目录名未必等于 module.prop 的 id（实测机型上目录名是仓库名），
# 故列出实际存在的候选目录，逐个找。
# 旧路径可被 THERMALD_LEGACY_DIR 覆盖（测试用，同 THERMALD_DIR 约定）
CANDIDATES="${THERMALD_LEGACY_DIR:-/data/adb/thermald}"
for d in /data/adb/modules/*/data; do
    [ -d "$d" ] || continue
    CANDIDATES="$CANDIDATES $d"
done
for PREV_DIR in $CANDIDATES; do
    [ "$PREV_DIR" = "$DATA_DIR" ] && continue
    for f in config.json override_mi_thermald; do
        if [ ! -f "$DATA_DIR/$f" ] && [ -f "$PREV_DIR/$f" ]; then
            cp "$PREV_DIR/$f" "$DATA_DIR/$f" \
                && ui_print "- 已从 $PREV_DIR 保留 $f"
        fi
    done
done

# 只首次安装落出厂配置；升级安装不得覆盖用户调过的参数
if [ ! -f "$DATA_DIR/config.json" ]; then
    # 契约 §4.4：defaults 只打印不落盘；落盘必须走 §4.5 的 set --reset
    THERMALD_DIR="$DATA_DIR" "$MODDIR/bin/thermald" set --reset >/dev/null 2>&1 \
        || ui_print "! 写入出厂配置失败，守护进程首次启动时会自行创建"
fi

ui_print "- 安装完成，重启后生效（数据目录: ${DATA_DIR}）"
