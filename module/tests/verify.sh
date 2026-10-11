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
    printf '#!/bin/sh\n[ "$1" = set ] && echo "{\\"version\\":1}" > "${THERMALD_DIR:-/data/adb/thermald}/config.json"\nexit 0\n' > module/bin/thermald
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

# 版本不一致必须报错
cp Cargo.toml "$TMPROOT/Cargo.toml.bak"
sed -i '' 's/^version = "0.1.0"/version = "9.9.9"/' Cargo.toml
if bash scripts/package.sh >"$TMPROOT/mismatch.out" 2>&1; then
    bad "版本不一致未报错"
else
    ok "版本不一致时退出码非 0"
    grep -q '版本不一致' "$TMPROOT/mismatch.out" && ok "报错信息含中文原因: $(cat "$TMPROOT/mismatch.out")" \
                                                 || bad "报错信息不可读"
fi
cp "$TMPROOT/Cargo.toml.bak" Cargo.toml
grep -q '0.1.0' Cargo.toml && ok "Cargo.toml 已还原" || bad "Cargo.toml 还原失败"

# ─────────────────────────────────────────────────────────
echo
echo "══════════════════════════════"
echo "  PASS=$PASS  FAIL=$FAIL"
echo "══════════════════════════════"
[ "$FAIL" = "0" ] || exit 1
