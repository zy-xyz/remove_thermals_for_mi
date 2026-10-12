#!/usr/bin/env bash
# B 组自测：KernelSU 模块封装（小类 1/2/3）全量验收
# 用法：bash module/tests/verify.sh
# 在仓库根运行；不依赖 KernelSU 与 Android 设备，全部在 macOS 本地跑通。
set -u

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"

PASS=0
FAIL=0
TMPROOT=$(mktemp -d)
# 只清理由本测试创建的文件；仓库里已有的产物（他人/CI 造的）一律不动
CREATED=""
cleanup() {
    pkill -f "$TMPROOT" 2>/dev/null || true
    for a in $CREATED; do rm -f "$a"; done
    rm -rf "$TMPROOT"
}
trap cleanup EXIT

ok()   { PASS=$((PASS+1)); echo "  PASS  $1"; }
bad()  { FAIL=$((FAIL+1)); echo "  FAIL  $1"; }
head_() { echo; echo "=== $1 ==="; }

# ─────────────────────────────────────────────────────────
head_ "小类1-①  module.prop 字段与格式校验"

PROP=module/module.prop
[ -f "$PROP" ] && ok "$PROP 存在" || bad "$PROP 不存在"

ID=$(sed -n 's/^id=//p' "$PROP" | head -1)
VC=$(sed -n 's/^versionCode=//p' "$PROP" | head -1)
VER=$(sed -n 's/^version=//p' "$PROP" | head -1)

if printf '%s' "$ID" | grep -Eq '^[a-zA-Z][a-zA-Z0-9._-]+$'; then
    ok "id 匹配官方正则 ^[a-zA-Z][a-zA-Z0-9._-]+\$ : id=$ID"
else
    bad "id 不匹配官方正则: '$ID'"
fi

if printf '%s' "$VC" | grep -Eq '^[0-9]+$'; then
    ok "versionCode 是整数: $VC"
else
    bad "versionCode 非整数: '$VC'"
fi

# versionCode == major*10000 + minor*100 + patch
EXPECT_VC=$(printf '%s' "$VER" | awk -F. '{printf "%d", $1*10000+$2*100+$3}')
[ "$VC" = "$EXPECT_VC" ] && ok "versionCode 符合公式 major*10000+minor*100+patch: $VER -> $VC" \
                         || bad "versionCode 不符公式: version=$VER 期望 ${EXPECT_VC}，实为 $VC"

for k in id name version versionCode author description; do
    if grep -q "^$k=." "$PROP"; then ok "字段 $k 非空"; else bad "字段 $k 缺失或为空"; fi
done

CARGO_VER=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
[ "$VER" = "$CARGO_VER" ] && ok "version 与 Cargo.toml 一致: $VER" \
                          || bad "version 不一致: module.prop=$VER Cargo.toml=$CARGO_VER"

# ─────────────────────────────────────────────────────────
head_ "小类1-②  customize.sh 架构校验（实测）"

if ARCH=x86_64 THERMALD_DIR="$TMPROOT/d1" sh module/customize.sh >"$TMPROOT/x86.out" 2>&1; then
    bad "ARCH=x86_64 未 abort（退出码为 0）"
else
    RC=$?
    ok "ARCH=x86_64 退出码非 0: $RC"
fi
echo "  --- 输出 ---"; sed 's/^/  | /' "$TMPROOT/x86.out"
grep -q '仅支持 arm64' "$TMPROOT/x86.out" && grep -q 'x86_64' "$TMPROOT/x86.out" \
    && ok "打印可读中文原因且回显实际架构" || bad "abort 信息不可读或未回显架构"

# ARCH=arm64 正常路径（需存在 bin/thermald）
mkdir -p "$TMPROOT/d2"
mkdir -p module/bin
if [ ! -f module/bin/thermald ]; then
    printf '#!/bin/sh\n[ "$1" = set ] && echo "{\\"version\\":1}" > "${THERMALD_DIR:-/data/adb/modules/thermald/data}/config.json"\nexit 0\n' > module/bin/thermald
    chmod 0755 module/bin/thermald
    CREATED="$CREATED module/bin/thermald"
fi
if ARCH=arm64 API=34 THERMALD_DIR="$TMPROOT/d2" sh module/customize.sh >"$TMPROOT/arm.out" 2>&1; then
    ok "ARCH=arm64 正常退出（码 0）"
else
    bad "ARCH=arm64 失败，退出码 $?；输出：$(cat "$TMPROOT/arm.out")"
fi
echo "  --- 输出 ---"; sed 's/^/  | /' "$TMPROOT/arm.out"
[ -d "$TMPROOT/d2" ] && ok "数据目录已创建: $TMPROOT/d2" || bad "数据目录未创建"
[ -f "$TMPROOT/d2/config.json" ] && ok "首次安装落默认配置 config.json" || bad "config.json 未生成"
# 升级安装不得覆盖已有配置
echo '{"setpoint":46.5}' > "$TMPROOT/d2/config.json"
ARCH=arm64 THERMALD_DIR="$TMPROOT/d2" sh module/customize.sh >/dev/null 2>&1
grep -q '46.5' "$TMPROOT/d2/config.json" && ok "升级安装不覆盖用户已有配置" || bad "升级安装覆盖了用户配置"

# ── 真机回归 ①：source 执行时 MODDIR 必须取自 MODPATH，而非 ${0%/*} ──
# KernelSU 是 `. $MODPATH/customize.sh` 执行的，$0 指向外层脚本。
# 曾因为用 ${0%/*} 导致 MODDIR 错、整个安装在真机上失败。本机自测用 sh 执行
# 会掩盖此 bug，故必须显式模拟 source 场景。
SRC="$TMPROOT/srcmod"; rm -rf "$SRC"; mkdir -p "$SRC/webroot"
cp module/module.prop module/customize.sh module/service.sh module/post-fs-data.sh module/uninstall.sh "$SRC/"
mkdir -p "$SRC/bin"; printf '#!/bin/sh\nexit 0\n' > "$SRC/bin/thermald"; chmod +x "$SRC/bin/thermald"
# 用 sh -c '. 脚本' 模拟 KernelSU：$0 是 sh，MODPATH 是唯一正确来源
if MODPATH="$SRC" ARCH=arm64 API=34 THERMALD_DIR="$TMPROOT/d3" sh -c '. "$1"' sh "$SRC/customize.sh" >"$TMPROOT/src.out" 2>&1; then
    grep -q "0.1.1" "$TMPROOT/src.out" && ok "source 执行时 MODPATH 生效（读到 module.prop 版本）" \
                                      || bad "source 执行时未读到 module.prop"
    [ -d "$TMPROOT/d3" ] && ok "source 执行时数据目录按 THERMALD_DIR 创建" \
                         || bad "source 执行时数据目录异常"
else
    bad "source 执行失败（MODDIR 取自 \${0%/*} 会导致此错）: $(cat "$TMPROOT/src.out")"
fi
# 未设 MODPATH 时应退回 ${0%/*}（本机直接 sh 执行场景仍须可用）
ARCH=arm64 THERMALD_DIR="$TMPROOT/d4" sh module/customize.sh >/dev/null 2>&1 \
    && ok "无 MODPATH 时退回 \${0%/*} 仍可安装" || bad "无 MODPATH 时安装失败"

# ── 真机回归 ②：升级须从旧固定路径 /data/adb/thermald 抢救配置 ──
# 0.1.0 的数据在 /data/adb/thermald，0.1.1 起改为模块目录下 data/。
# 真机上模块目录名还会与 module.prop 的 id 不同，故用目录枚举而非拼 id。
LEG="$TMPROOT/legacy"; rm -rf "$LEG"; mkdir -p "$LEG"
echo '{"setpoint":45.5,"用户的调参":true}' > "$LEG/config.json"
touch "$LEG/override_mi_thermald"
echo '{"stale":1}' > "$LEG/state.json"
ARCH=arm64 THERMALD_DIR="$TMPROOT/d5" THERMALD_LEGACY_DIR="$LEG" sh module/customize.sh >"$TMPROOT/mig.out" 2>&1
grep -q '45.5' "$TMPROOT/d5/config.json" 2>/dev/null && ok "升级从旧固定路径抢救 config.json" \
                                               || bad "未从旧固定路径抢救 config.json"
[ -f "$TMPROOT/d5/override_mi_thermald" ] && ok "升级从旧固定路径抢救 override 开关" \
                                          || bad "未抢救 override 开关"
[ -f "$TMPROOT/d5/state.json" ] && bad "运行态 state.json 不该被迁移" || ok "运行态 state.json 正确未迁移"

# ─────────────────────────────────────────────────────────
head_ "小类1-③  换行符与语法检查"

for f in module/module.prop module/customize.sh module/service.sh module/post-fs-data.sh \
         module/uninstall.sh module/sepolicy.rule scripts/package.sh; do
    if [ -f "$f" ]; then
        if LC_ALL=C grep -q $'\r' "$f" 2>/dev/null; then bad "$f 含 CR（非 LF）"; else ok "$f 为纯 LF"; fi
    else
        bad "$f 不存在"
    fi
done
echo "  --- file 输出 ---"
file module/customize.sh module/service.sh module/post-fs-data.sh module/uninstall.sh scripts/package.sh | sed 's/^/  | /'

for f in module/customize.sh module/service.sh module/post-fs-data.sh module/uninstall.sh; do
    if sh -n "$f" 2>"$TMPROOT/syn.err"; then ok "sh -n $f 通过"; else bad "sh -n $f 失败: $(cat "$TMPROOT/syn.err")"; fi
done
bash -n scripts/package.sh && ok "bash -n scripts/package.sh 通过" || bad "bash -n scripts/package.sh 失败"

# bash 3.2（macOS 自带）在 "$VAR 紧跟多字节字符" 时会吞掉展开并把该字符截成半个，
# 产生非法 UTF-8 的错误信息（实测：echo "（$V）" 输出 "\xbc\x89"）。
# 必须用 ${VAR} 花括号形式。此处静态扫描，防止回归。
MBHIT=$(python3 - <<'PY'
import re, pathlib
pat = re.compile(r'\$([A-Za-z_][A-Za-z0-9_]*)(?=[^\x00-\x7f])')
hits = []
for p in sorted(pathlib.Path('.').rglob('*')):
    if not p.is_file() or p.suffix not in ('.sh', '.yml', '.yaml'):
        continue
    if any(s in p.parts for s in ('.git', 'target', 'build')):
        continue
    for i, line in enumerate(p.read_text(encoding='utf-8').splitlines(), 1):
        # 注释行不执行，跳过；否则下方说明里的示例写法会被误报
        if line.lstrip().startswith('#'):
            continue
        if pat.search(line):
            hits.append(f"{p}:{i}")
print(",".join(hits))
PY
)
[ -z "$MBHIT" ] && ok "无 '\$VAR 紧跟中文' 写法（bash 3.2 会产出非法 UTF-8）" \
                || bad "存在会被 bash 3.2 截断的写法，请改用 \${VAR}: $MBHIT"

# ─────────────────────────────────────────────────────────
head_ "小类1-④  uninstall.sh 清数据目录 + 杀进程 + 容错（实测）"

UN="$TMPROOT/unin"
mkdir -p "$UN/bin" "$UN/data/sub"
printf '#!/bin/sh\necho fake\nsleep 30\n' > "$UN/bin/thermald"; chmod 0755 "$UN/bin/thermald"
# 造全套状态文件（契约 §2：配置/状态/日志/CSV/锁）
for f in config.json state.json thermald.log history.csv thermald.lock; do echo x > "$UN/data/$f"; done
"$UN/bin/thermald" daemon >/dev/null 2>&1 &
FAKE_PID=$!
sleep 0.5
if kill -0 "$FAKE_PID" 2>/dev/null; then ok "已启动假守护进程 pid=${FAKE_PID}（用于验证被杀）"; else bad "假守护进程未启动"; fi

THERMALD_DIR="$UN/data" sh module/uninstall.sh && ok "uninstall.sh 退出码 0" || bad "uninstall.sh 退出码非 0"
[ ! -d "$UN/data" ] && ok "数据目录已整体删除（配置/状态/日志/CSV/锁全清）" || bad "数据目录残留: $(ls -A "$UN/data" 2>/dev/null)"
sleep 0.5
kill -0 "$FAKE_PID" 2>/dev/null && bad "残留进程未被杀: pid=$FAKE_PID" || ok "残留守护进程已被杀: pid=$FAKE_PID"

# 容错：目录与进程都不存在时，重复执行仍须退出 0（pkill 未命中不算错）
THERMALD_DIR="$UN/data" sh module/uninstall.sh && ok "容错：目标不存在时重复执行仍退出 0" || bad "容错失败：目标不存在时退出码非 0"

# ─────────────────────────────────────────────────────────
head_ "小类2-①  service.sh 拉起守护进程后立即返回（实测）"

SD="$TMPROOT/svc"
mkdir -p "$SD/bin" "$SD/data" "$SD/stub"
printf '#!/bin/sh\necho "fake thermald daemon pid=$$"\nsleep 30\n' > "$SD/bin/thermald"
chmod 0755 "$SD/bin/thermald"
printf '#!/bin/sh\necho 1\n' > "$SD/stub/getprop"
chmod 0755 "$SD/stub/getprop"
cp module/service.sh "$SD/service.sh"

START=$(date +%s.%N)
PATH="$SD/stub:$PATH" THERMALD_DIR="$SD/data" sh "$SD/service.sh" &
SCRIPT_PID=$!
wait "$SCRIPT_PID"; RC=$?
END=$(date +%s.%N)
ELAPSED=$(printf '%s' "$END $START" | awk '{printf "%.3f", $1-$2}')

[ "$RC" = "0" ] && ok "service.sh 退出码 0" || bad "service.sh 退出码 $RC"
awk -v e="$ELAPSED" 'BEGIN{exit !(e<1.0)}' \
    && ok "service.sh 返回耗时 ${ELAPSED}s < 1s（不含守护进程自身运行）" \
    || bad "service.sh 返回耗时 ${ELAPSED}s，未满足 < 1s"

DAEMON_PID=$(pgrep -f "$SD/bin/thermald daemon" | head -1)
# service.sh 按设计「拉起后立即返回」，首次探测可能撞上刚返回的瞬间，故重试至多 3s。
i=0
while [ -z "$DAEMON_PID" ] && [ "$i" -lt 10 ]; do
    sleep 0.3
    DAEMON_PID=$(pgrep -f "$SD/bin/thermald daemon" | head -1)
    i=$((i+1))
done
# pgrep -f 会匹配到本测试脚本自身命令行（其中含该模式），需排除自身 pid。
[ "$DAEMON_PID" = "$$" ] && DAEMON_PID=$(pgrep -f "$SD/bin/thermald daemon" | grep -v "^$$\$" | head -1)
if [ -n "$DAEMON_PID" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    ok "守护进程仍在运行: pid=${DAEMON_PID}（证明父脚本已返回而子进程存活）"
else
    bad "未找到存活的守护进程"
    DAEMON_PID=""
fi

if kill -0 "$SCRIPT_PID" 2>/dev/null; then
    bad "service.sh 进程仍存活（pid=${SCRIPT_PID}）"
else
    ok "service.sh 进程已退出（pid=$SCRIPT_PID 已消失）"
fi
if [ -n "$DAEMON_PID" ]; then
    PPID_V=""
    if ps -o ppid= -p "$DAEMON_PID" >"$TMPROOT/ppid.txt" 2>/dev/null; then
        PPID_V=$(tr -d ' ' <"$TMPROOT/ppid.txt")
    fi
    if [ -n "$PPID_V" ]; then
        [ "$PPID_V" != "$SCRIPT_PID" ] \
            && ok "守护进程已脱离会话: ppid=$PPID_V != 脚本 pid=$SCRIPT_PID" \
            || bad "守护进程父进程仍是脚本 pid=$SCRIPT_PID"
    else
        # 本机沙箱拒绝 /bin/ps；退路：脚本 pid 已消失且守护进程仍存活 → 其父必非脚本
        ok "守护进程已脱离会话（ppid 不可查：本机 /bin/ps 被沙箱拒绝）；脚本 pid=$SCRIPT_PID 已消失而 pid=$DAEMON_PID 存活，故其父必非脚本"
    fi
fi
echo "  --- 日志 ---"; sed 's/^/  | /' "$SD/data/thermald.log" 2>/dev/null

pkill -f "$SD" 2>/dev/null || true

# ─────────────────────────────────────────────────────────
head_ "小类2-②  service.sh 覆盖开关两态行为差异"

# 关闭态：默认不写 /system
SD2="$TMPROOT/svc2"
mkdir -p "$SD2/bin" "$SD2/data" "$SD2/stub"
printf '#!/bin/sh\nsleep 30\n' > "$SD2/bin/thermald"; chmod 0755 "$SD2/bin/thermald"
printf '#!/bin/sh\necho 1\n' > "$SD2/stub/getprop"; chmod 0755 "$SD2/stub/getprop"
cp module/service.sh "$SD2/service.sh"
PATH="$SD2/stub:$PATH" THERMALD_DIR="$SD2/data" sh "$SD2/service.sh"
[ ! -d "$SD2/system" ] && ok "开关关闭: 模块内无 system/ 目录（不触发 /system 挂载）" \
                       || bad "开关关闭却创建了 $SD2/system"
grep -q '开关关闭，未向 /system 写入任何文件' "$SD2/data/thermald.log" \
    && ok "开关关闭: 日志明确记录未写 /system" || bad "开关关闭: 日志缺少说明"
pkill -f "$SD2" 2>/dev/null || true

# 开启态：写入模块内 system/bin/mi_thermald（占位），需 metamodule 才生效
touch "$SD2/data/override_mi_thermald"
PATH="$SD2/stub:$PATH" THERMALD_DIR="$SD2/data" sh "$SD2/service.sh"
if [ -f "$SD2/system/bin/mi_thermald" ]; then
    ok "开关开启: 已生成占位 $SD2/system/bin/mi_thermald"
    ls -l "$SD2/system/bin/mi_thermald" | sed 's/^/  | /'
    [ -x "$SD2/system/bin/mi_thermald" ] && ok "占位脚本可执行" || bad "占位脚本不可执行"
    grep -q '需 metamodule' "$SD2/data/thermald.log" && ok "日志注明需 metamodule + 重启" \
                                                       || bad "日志未注明 metamodule 前提"
else
    bad "开关开启但未生成占位脚本"
fi
pkill -f "$SD2" 2>/dev/null || true

# 脚本内所有 /system 引用均为只读路径，无写入重定向
if grep -nE '>\s*/system|>>\s*/system|cp .*/system/|install .*/system/' module/service.sh >"$TMPROOT/sysw.log" 2>&1; then
    bad "service.sh 存在对 /system 的直写: $(cat "$TMPROOT/sysw.log")"
else
    ok "service.sh 中无任何直写 /system 的重定向（覆盖仅经模块内 system/ 由 overlayfs 生效）"
fi

# ─────────────────────────────────────────────────────────
head_ "小类2-③  post-fs-data.sh 极快返回"

[ -s module/post-fs-data.sh ] && ok "post-fs-data.sh 非空" || bad "post-fs-data.sh 为空"
# 只在「去掉注释行」后检查，避免注释里提到 sleep 造成误报
if sed 's/#.*//' module/post-fs-data.sh | grep -qE 'sleep|until |while '; then
    bad "post-fs-data.sh 含 sleep/循环，违反阻塞钩子约束"
else
    ok "post-fs-data.sh 无 sleep / 无循环（已剥离注释行后检查）"
fi
T0=$(date +%s.%N)
THERMALD_DIR="$TMPROOT/pfd" sh module/post-fs-data.sh
T1=$(date +%s.%N)
printf '  返回耗时 %ss\n' "$(printf '%s' "$T1 $T0" | awk '{printf "%.3f", $1-$2}')"
[ -d "$TMPROOT/pfd" ] && ok "数据目录已创建" || bad "数据目录未创建"

# ─────────────────────────────────────────────────────────
head_ "小类3-①  scripts/package.sh 打包（实测）"

mkdir -p build
if [ ! -f build/thermald ]; then
    printf '#!/bin/sh\necho fake aarch64 binary\n' > build/thermald
    chmod 0755 build/thermald
    CREATED="$CREATED build/thermald"
fi
# 注意：绝不向 webroot/ 写文件——那是 C 组作用域。C 组未产出时打包应「警告并跳过」。
WEBROOT_READY=0
if [ -d webroot ] && [ -n "$(ls -A webroot 2>/dev/null)" ]; then WEBROOT_READY=1; fi
echo "  (webroot 就绪: $WEBROOT_READY)"
bash scripts/package.sh > "$TMPROOT/pkg.out" 2>"$TMPROOT/pkg.err"
echo "  --- 输出 ---"; sed 's/^/  | /' "$TMPROOT/pkg.out"; sed 's/^/  | /' "$TMPROOT/pkg.err"
VER_NOW=$(sed -n 's/^version=//p' module/module.prop | head -1)
ZIP="build/thermald-v$VER_NOW.zip"
CREATED="$CREATED $ZIP"
[ -f "$ZIP" ] && ok "产物存在: $ZIP" || bad "产物不存在: $ZIP"

echo "  --- unzip -l ---"; unzip -l "$ZIP" | sed 's/^/  | /'

ENTRIES="module.prop customize.sh service.sh post-fs-data.sh uninstall.sh sepolicy.rule bin/thermald"
[ "$WEBROOT_READY" = "1" ] && ENTRIES="$ENTRIES webroot/index.html"
for e in $ENTRIES; do
    unzip -Z1 "$ZIP" | grep -qx "$e" && ok "zip 顶层含 $e" || bad "zip 缺少 $e"
done
if [ "$WEBROOT_READY" = "0" ]; then
    grep -q '跳过 WebUI' "$TMPROOT/pkg.err" && ok "webroot 缺失时仅警告不失败（符合契约：C 组可并行开发）" \
                                             || bad "webroot 缺失时未给出跳过警告"
fi
if unzip -Z1 "$ZIP" | grep -qE '^(module|\.)/'; then
    bad "zip 含外层目录（出现 module/ 或 ./ 前缀）"
else
    ok "zip 无外层目录：所有条目均顶层直出"
fi

rm -rf "$TMPROOT/unz"; mkdir -p "$TMPROOT/unz"
unzip -q "$ZIP" -d "$TMPROOT/unz"
PERM=$(ls -l "$TMPROOT/unz/bin/thermald" | awk '{print $1}')
[ "$PERM" = "-rwxr-xr-x" ] && ok "解包后 bin/thermald 权限 0755 ($PERM)" || bad "bin/thermald 权限异常: $PERM"
unzip -Z "$ZIP" 2>/dev/null | grep 'bin/thermald' | sed 's/^/  | /'

# 版本不一致必须报错（真实版本从 Cargo.toml 读，避免升版本后本测试失效）
cp Cargo.toml "$TMPROOT/Cargo.toml.bak"
REAL_VER=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
sed -i '' "s/^version = \"$REAL_VER\"/version = \"9.9.9\"/" Cargo.toml
if bash scripts/package.sh >"$TMPROOT/mismatch.out" 2>&1; then
    bad "版本不一致未报错"
else
    ok "版本不一致时退出码非 0"
    grep -q '版本不一致' "$TMPROOT/mismatch.out" && ok "报错信息含中文原因: $(cat "$TMPROOT/mismatch.out")" \
                                                 || bad "报错信息不可读"
fi
cp "$TMPROOT/Cargo.toml.bak" Cargo.toml
grep -q "^version = \"$REAL_VER\"" Cargo.toml && ok "Cargo.toml 已还原 ($REAL_VER)" || bad "Cargo.toml 还原失败"

# ─────────────────────────────────────────────────────────
head_ "小类3-②  在线更新元数据 update.json（实测）"
# 依据：KernelSU ModuleRepositoryImpl.checkUpdate 读 {version,versionCode,zipUrl,changelog}，
# 且仅当 versionCode > 已装版本才提示更新。字段名写错 = 静默失效，故逐个断言。

UJ=build/update.json
[ -f "$UJ" ] && ok "产物存在: $UJ" || bad "产物不存在: $UJ"

# 关键：KSU 读的是已安装模块里的 module.prop，zip 内那份必须带 updateJson
unzip -p "$ZIP" module.prop > "$TMPROOT/zip.prop" 2>/dev/null
ZIP_UJ=$(sed -n 's/^updateJson=//p' "$TMPROOT/zip.prop" | head -1)
[ -n "$ZIP_UJ" ] && ok "zip 内 module.prop 带 updateJson，管理器能发现更新源" \
                 || bad "zip 内 module.prop 缺 updateJson（管理器不会检查更新）"
case "$ZIP_UJ" in
    https://*/update.json) ok "updateJson 是 https 且以 /update.json 结尾" ;;
    *) bad "updateJson 格式异常: '$ZIP_UJ'" ;;
esac

# 用 python3 解析，避免用正则去猜 JSON 结构
UJ_OUT=$(python3 - "$UJ" "$PROP" <<'PY' 2>&1
import json, re, sys
uj, prop = sys.argv[1], sys.argv[2]
u = json.load(open(uj))
p = open(prop).read()
g = lambda k: re.search(rf'^{k}=(.*)$', p, re.M).group(1).strip()
errs = []
# KSU 只认这四个键；缺 zipUrl 或 versionCode<=0 会被判为「无更新」
for k in ("version", "versionCode", "zipUrl", "changelog"):
    if k not in u:
        errs.append(f"缺少字段 {k}")
if u.get("version") != g("version"):
    errs.append(f"version 不一致: {u.get('version')} vs {g('version')}")
if u.get("versionCode") != int(g("versionCode")):
    errs.append(f"versionCode 不一致: {u.get('versionCode')} vs {g('versionCode')}")
if u.get("versionCode", 0) <= 0:
    errs.append("versionCode <= 0，KSU 会忽略该更新")
# zipUrl 必须指向本次版本的资产，否则检查到新版却下载到旧包
if not str(u.get("zipUrl", "")).endswith(f"thermald-v{g('version')}.zip"):
    errs.append(f"zipUrl 未指向本版本资产: {u.get('zipUrl')}")
if not u.get("changelog"):
    errs.append("changelog 为空")
print("; ".join(errs) if errs else "ok")
PY
)
if [ "$UJ_OUT" = "ok" ]; then
    ok "update.json 字段名/版本/下载地址自洽（KSU schema）"
else
    bad "update.json 校验失败: $UJ_OUT"
fi
echo "  --- update.json ---"; sed 's/^/  | /' "$UJ"

# 负向：module.prop 去掉 updateJson 时必须打包失败（否则发版会静默丢掉更新能力）
cp "$PROP" "$TMPROOT/module.prop.bak"
grep -v '^updateJson=' "$TMPROOT/module.prop.bak" > "$PROP"
if bash scripts/package.sh >"$TMPROOT/nouj.out" 2>&1; then
    bad "缺 updateJson 时未报错"
else
    ok "缺 updateJson 时退出码非 0"
    grep -q 'updateJson' "$TMPROOT/nouj.out" && ok "报错信息指明 updateJson" || bad "报错信息未提及 updateJson"
fi
cp "$TMPROOT/module.prop.bak" "$PROP"
grep -q '^updateJson=' "$PROP" && ok "module.prop 已还原" || bad "module.prop 还原失败"

# ─────────────────────────────────────────────────────────
echo
echo "══════════════════════════════"
echo "  PASS=$PASS  FAIL=$FAIL"
echo "══════════════════════════════"
[ "$FAIL" = "0" ] || exit 1
