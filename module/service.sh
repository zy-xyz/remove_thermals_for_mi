#!/system/bin/sh
# KernelSU late_start 服务脚本：等开机完成后拉起守护进程，然后**立即返回**
set -e

MODDIR=${0%/*}
BIN=$MODDIR/bin/thermald
# 契约 §2：数据目录可用 THERMALD_DIR 覆盖（测试用）
DATA_DIR=${THERMALD_DIR:-/data/adb/thermald}
LOG=$DATA_DIR/thermald.log

mkdir -p "$DATA_DIR"
chmod 0700 "$DATA_DIR"

# ── 可选覆盖开关：默认关闭，绝不触碰 /system ──
# 打开方式：touch /data/adb/thermald/override_mi_thermald 后重启。
# 原理：把占位脚本写进模块的 system/bin/mi_thermald，由 KernelSU 的 overlayfs
# 叠加到真实 /system/bin/mi_thermald 之上，使 MIUI 原厂温控调用变成空操作。
# 前提：必须已安装 metamodule（如 meta-overlayfs），否则 KernelSU 不会挂载 /system，
#       写了文件也不生效；且 overlay 变更需**重启**后才可见（服务脚本处于 late_start，
#       此时 overlayfs 早已挂载完毕，无法热更新 /system）。
OVERRIDE_FLAG=$DATA_DIR/override_mi_thermald
OVERRIDE_PATH=$MODDIR/system/bin/mi_thermald

if [ -f "$OVERRIDE_FLAG" ]; then
    mkdir -p "$MODDIR/system/bin"
    cat > "$OVERRIDE_PATH" <<'SHIM'
#!/system/bin/sh
# 占位：使 MIUI 原厂 mi_thermald 调用变为空操作，控制权交给本模块守护进程。
echo "$(date +%s) mi_thermald 调用已被本模块占位($*)" >> /data/adb/thermald/thermald.log
exit 0
SHIM
    chmod 0755 "$OVERRIDE_PATH"
    echo "$(date +%s) override: 已启用 /system/bin/mi_thermald 占位（需 metamodule + 重启生效）" >> "$LOG"
else
    # 开关关闭：确保模块内不存在 system/ 目录，避免 KernelSU 误挂载 /system
    rm -rf "$MODDIR/system"
    echo "$(date +%s) override: 开关关闭，未向 /system 写入任何文件" >> "$LOG"
fi

# ── 等开机完成：守护进程要读电源 sysfs，需等电源服务就绪 ──
until [ "$(getprop sys.boot_completed)" = "1" ]; do
    sleep 1
done

# ── 崩溃重启兜底（Watchdog）──
# 理由：守护进程编译时 panic=abort，被 OOM killer 杀掉或异常退出后若不重启，
# 手机会静默失去温控（充电电流留在最后一次写入值），故保留 30s 轮询兜底。
# 用 pgrep 先判断，避免与「已有实例则退出 0」的单实例锁形成 5s 一次的忙重启循环。
# 整个 watchdog 放进后台子 shell：守护进程的父进程是 watchdog，不是本脚本，
# 本脚本退出后 watchdog 与守护进程均被 init 收养，不阻塞开机。
(
    # 子 shell 内关闭 set -e：pkill/pgrep 未命中属正常情况
    set +e
    while true; do
        if [ -z "$(pgrep -f 'bin/thermald daemon' 2>/dev/null)" ]; then
            nohup "$BIN" daemon >>"$LOG" 2>&1 &
        fi
        sleep 30
    done
) </dev/null >>"$LOG" 2>&1 &

exit 0
