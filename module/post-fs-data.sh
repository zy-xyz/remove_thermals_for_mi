#!/system/bin/sh
# KernelSU post-fs-data 钩子：阻塞开机流程（超 10s 会中止启动），必须极快返回。
# 只做早期准备：把数据目录建好并收紧权限，让随后的 service.sh 与 CLI 都能直接用。
set -e

MODDIR=${0%/*}
# 数据目录：模块目录下 data/，随模块走（卸载即清）
DATA_DIR=${THERMALD_DIR:-$MODDIR/data}

mkdir -p "$DATA_DIR"
chown 0:0 "$DATA_DIR" 2>/dev/null || true
chmod 0700 "$DATA_DIR"

# 无 sleep、无网络、无外部命令等待 —— 见上
exit 0
