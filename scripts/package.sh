#!/usr/bin/env bash
# 打 KernelSU 模块 zip（契约 §9）
#   输入：build/thermald（不存在则先调 scripts/build.sh）
#   输出：build/thermald-v<version>.zip
#   zip 顶层 == 模块根，无外层目录
set -e

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

MODULE_DIR=module
PROP=$MODULE_DIR/module.prop
BUILD_DIR=build
BIN=$BUILD_DIR/thermald

die() { echo "错误: $*" >&2; exit 1; }

# ── 版本单点：module.prop ──
[ -f "$PROP" ] || die "缺少 $PROP"
VERSION=$(sed -n 's/^version=//p' "$PROP" | head -1)
[ -n "$VERSION" ] || die "$PROP 中没有 version="

# ── 版本一致性断言（与 scripts/build.sh 同一约束）──
CARGO_VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$CARGO_VERSION" ] || die "Cargo.toml 中没有 version"
if [ "$VERSION" != "$CARGO_VERSION" ]; then
    die "版本不一致：module/module.prop version=${VERSION}，Cargo.toml version=${CARGO_VERSION}"
fi

# ── 在线更新元数据：module.prop 是版本单点，update.json 由它派生 ──
# KernelSU 管理器拉 module.prop 的 updateJson → 读 {version,versionCode,zipUrl,changelog}，
# 仅当 versionCode > 已装版本才提示更新（见 KSU ModuleRepositoryImpl.checkUpdate）。
VERSION_CODE=$(sed -n 's/^versionCode=//p' "$PROP" | head -1)
[ -n "$VERSION_CODE" ] || die "$PROP 中没有 versionCode="
# module.prop 的 updateJson 同时充当「仓库 + 代理前缀」的单点：zipUrl 由它派生，
# 换仓库或换代理只改 module.prop 一处。故这里强校验其完整形态。
UPDATE_JSON=$(sed -n 's/^updateJson=//p' "$PROP" | head -1)
[ -n "$UPDATE_JSON" ] || die "$PROP 中没有 updateJson="
case "$UPDATE_JSON" in
    */releases/latest/download/update.json) ;;
    *) die "module.prop 的 updateJson 须形如 <前缀>/releases/latest/download/update.json（当前: ${UPDATE_JSON}）" ;;
esac
UPD_PREFIX="${UPDATE_JSON%/releases/latest/download/update.json}"

# 资产下载地址用 tag 固定地址，而非 <前缀>/releases/latest/download/。
# latest 会随新版本移动，与 update.json 被代理缓存的时间点可能错开，
# 出现「update.json 已说 0.1.1、latest 已翻到 0.1.2」而下载到错版本的窗口。
# tag 地址不可变（实测同样可达），保证更新检查拿到自洽的一对。
ZIP_URL="$UPD_PREFIX/releases/download/v$VERSION/thermald-v$VERSION.zip"

# ── 输入二进制：缺失则交给 scripts/build.sh ──
if [ ! -f "$BIN" ]; then
    echo "- $BIN 不存在，调用 scripts/build.sh 构建"
    [ -f scripts/build.sh ] || die "缺少 scripts/build.sh，无法构建 $BIN"
    bash scripts/build.sh
    [ -f "$BIN" ] || die "scripts/build.sh 执行后仍未产出 $BIN"
fi

OUT=$BUILD_DIR/thermald-v$VERSION.zip
STAGE=$BUILD_DIR/.stage

# ── 暂存目录：迁移到暂存再打包，保证 zip 无外层目录 ──
rm -rf "$STAGE"
mkdir -p "$STAGE/bin"

for f in module.prop customize.sh service.sh post-fs-data.sh uninstall.sh sepolicy.rule; do
    [ -f "$MODULE_DIR/$f" ] || die "缺少模块文件 $MODULE_DIR/$f"
    cp "$MODULE_DIR/$f" "$STAGE/$f"
done

cp "$BIN" "$STAGE/bin/thermald"
chmod 0755 "$STAGE/bin/thermald"

# ── WebUI（C 组产出，可能尚未就绪：警告并跳过，不算失败）──
if [ -d webroot ] && [ -n "$(ls -A webroot 2>/dev/null)" ]; then
    mkdir -p "$STAGE/webroot"
    cp -R webroot/. "$STAGE/webroot/"
else
    echo "警告: 仓库根 webroot/ 不存在或为空，本次打包跳过 WebUI（C 组可能仍在开发）" >&2
fi

# ── 打包：cd 进暂存目录，用 . 作源路径，顶层即模块根 ──
rm -f "$OUT"
( cd "$STAGE" && zip -X -r -q "$ROOT/$OUT" . )

rm -rf "$STAGE"
echo "已生成 $OUT"

# ── update.json：Release 资产，供 KSU 管理器在线更新 ──
# 字段名以 KSU 源码 ModuleRepositoryImpl.checkUpdate 为准：
#   version / versionCode / zipUrl / changelog（versionCode 非整数或 zipUrl 空则被忽略）
# 作为 Release asset 单独上传，不进模块 zip（模块内不需要它）。
UPDATE_JSON_OUT=$BUILD_DIR/update.json
cat > "$UPDATE_JSON_OUT" <<JSON
{
  "version": "$VERSION",
  "versionCode": $VERSION_CODE,
  "zipUrl": "$ZIP_URL",
  "changelog": "详见 https://github.com/zy-xyz/remove_thermals_for_mi/releases/tag/v$VERSION"
}
JSON
python3 -c "import json,sys; json.load(open('$UPDATE_JSON_OUT'))" \
    || die "生成的 $UPDATE_JSON_OUT 不是合法 JSON"
echo "已生成 $UPDATE_JSON_OUT"
