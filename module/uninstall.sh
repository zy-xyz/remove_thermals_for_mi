#!/system/bin/sh
# KernelSU 卸载钩子：模块被移除时执行
# 职责：杀残留进程 → 清数据目录（状态/配置/日志/CSV/锁文件）
# 注意：本脚本以 post-fs-data 模式运行，必须快速返回。
set -e

DATA_DIR=${THERMALD_DIR:-/data/adb/thermald}

# ── 杀残留守护进程（容错：进程不存在返回非 0，故加 || true）──
# 先 TERM 给主循环机会复位充电电流，再 KILL 兜底
pkill -f '/data/adb/modules/.*/bin/thermald' 2>/dev/null || true
pkill -f 'bin/thermald daemon' 2>/dev/null || true
sleep 1
pkill -9 -f '/data/adb/modules/.*/bin/thermald' 2>/dev/null || true
pkill -9 -f 'bin/thermald daemon' 2>/dev/null || true

# ── 清数据目录：整个删掉，不留状态/配置/日志/CSV/锁文件 ──
rm -rf "$DATA_DIR"
