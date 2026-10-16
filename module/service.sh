#!/system/bin/sh
# KernelSU late_start 服务脚本：等开机完成后拉起守护进程，然后**立即返回**
set -e

MODDIR=${0%/*}
BIN=$MODDIR/bin/thermald
# 数据目录：模块目录下 data/，随模块走（卸载即清）
# 契约 §2：可用 THERMALD_DIR 覆盖（测试用）
DATA_DIR=${THERMALD_DIR:-$MODDIR/data}
LOG=$DATA_DIR/thermald.log
LOCK=$DATA_DIR/thermald.lock

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
    cat > "$OVERRIDE_PATH" <<SHIM
#!/system/bin/sh
# 占位：使 MIUI 原厂 mi_thermald 调用变为空操作，控制权交给本模块守护进程。
echo "\$(date +%s) mi_thermald 调用已被本模块占位(\$*)" >> $DATA_DIR/thermald.log
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

# ── 日志轮转 ──
# 必须做：守护进程若陷入「启动即退出」循环，会把日志无限刷大
# （实测一次故障刷出 41MB / 59 万行），最终撑爆 /data。
# 就地截断（保持同一 inode），避免打断 watchdog 子 shell 已持有的 fd。
LOG_MAX_BYTES=1048576
rotate_log() {
    [ -f "$LOG" ] || return 0
    sz=$(wc -c < "$LOG" 2>/dev/null || echo 0)
    [ "$sz" -gt "$LOG_MAX_BYTES" ] || return 0
    tail -n 200 "$LOG" > "$LOG.tmp" 2>/dev/null &&
        cat "$LOG.tmp" > "$LOG" 2>/dev/null
    rm -f "$LOG.tmp" 2>/dev/null
    echo "$(date +%s) service: 日志超限已轮转（保留最近 200 行）" >> "$LOG"
    return 0
}

# ── 崩溃重启兜底（Watchdog）──
# 理由：守护进程编译时 panic=abort，被 OOM killer 杀掉或异常退出后若不重启，
# 手机会静默失去温控（充电电流留在最后一次写入值），故保留 30s 轮询兜底。
#
# 判据不能用 `pgrep -f 'bin/thermald daemon'`：该模式既依赖相对路径（与 $BIN
# 的实际绝对路径不一致），又会被任何命令行里含该字样的进程误命中，
# 且守护进程「秒退」时它永远命中不到 —— 实测导致每 2~3 秒重生一次、
# 19 小时刷出 59 万行日志并烧掉 2 小时 CPU。
# 改为校验锁文件里的 PID 是否**确实是 thermald**（防 PID 复用），
# 与 daemon 侧 LockGuard 的判据保持一致。
#
# 整个 watchdog 放进后台子 shell：守护进程的父进程是 watchdog，不是本脚本，
# 本脚本退出后 watchdog 与守护进程均被 init 收养，不阻塞开机。
daemon_alive() {
    [ -f "$LOCK" ] || return 1
    p=$(cat "$LOCK" 2>/dev/null)
    # 锁内容必须是纯数字 PID
    case "$p" in
        ''|*[!0-9]*) return 1 ;;
    esac
    [ -d "/proc/$p" ] || return 1
    # 身份校验：PID 可能已被内核复用给无关进程，必须确认是 thermald。
    tr '\0' ' ' < "/proc/$p/cmdline" 2>/dev/null | grep -q 'thermald' || return 1
    return 0
}

rotate_log
(
    # 子 shell 内关闭 set -e：判据未命中属正常情况
    set +e
    while true; do
        if ! daemon_alive; then
            # 必须把 DATA_DIR 显式传给守护进程：二进制内建的默认路径是
            # /data/adb/modules/thermald/data，而模块目录名未必等于 module.prop
            # 的 id（实测机型上目录名是仓库名 remove_thermals_for_mi），
            # 不传就会写到不存在的路径。
            THERMALD_DIR="$DATA_DIR" nohup "$BIN" daemon >>"$LOG" 2>&1 &
        fi
        rotate_log
        sleep 30
    done
) </dev/null >>"$LOG" 2>&1 &

exit 0
