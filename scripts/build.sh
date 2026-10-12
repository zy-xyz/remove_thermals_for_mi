#!/usr/bin/env bash
# 交叉编译出 aarch64-linux-musl 静态二进制 build/thermald（契约 §9/§11）。
#
# 前置：Rust stable + aarch64-unknown-linux-musl target（无需 zig）
# 输入：Cargo.toml、src/
# 输出：build/thermald（真静态、已 strip）、build/thermald.sha256
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

die() { echo "错误: $*" >&2; exit 1; }
die3() { echo "错误: $*" >&2; exit 3; }
info() { echo "› $*"; }

# ── 版本单点：以 module/module.prop 为准（契约 §11）──
PROP="module/module.prop"
[ -f "$PROP" ] || die "缺少 ${PROP}（B 组产出）"
MODULE_VERSION=$(sed -n 's/^version=//p' "$PROP" | head -1 | tr -d '[:space:]')
[ -n "$MODULE_VERSION" ] || die "$PROP 中没有 version="

[ -f Cargo.toml ] || die "缺少 Cargo.toml"
CARGO_VERSION=$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1 | tr -d '[:space:]')
[ -n "$CARGO_VERSION" ] || die "Cargo.toml 中没有 version="
if [ "$MODULE_VERSION" != "$CARGO_VERSION" ]; then
  die "版本不一致：module/module.prop version=${MODULE_VERSION}，Cargo.toml version=${CARGO_VERSION}（契约 §11 要求单点一致）"
fi
info "版本：$MODULE_VERSION"

# ── 硬约束：零第三方 crate（契约 §0）──
if awk '
  /^\[dependencies\]/ { on=1; next }
  /^\[/               { on=0 }
  on && /^[[:space:]]*[A-Za-z0-9_-]+[[:space:]]*=/ { found=1 }
  END { exit !found }
' Cargo.toml; then
  die "契约 §0 硬约束：[dependencies] 必须为空；本机离线，目标机要全静态，禁用 clap/serde/anyhow 等第三方 crate"
fi

# ── Rust target 检查 ──
TARGET="aarch64-unknown-linux-musl"
if command -v rustup >/dev/null 2>&1 && ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
  cat >&2 <<EOF
错误: 缺少 Rust target: ${TARGET}。修复方式:
  rustup target add $TARGET
EOF
  exit 3
fi
info "Rust target: $TARGET"

# ── 链接器：用 rust 自带的 rust-lld ──
#
# 为什么不用 zig cc 当 linker：zig 0.13 的 LLD 解析不了新版 rustc 产出的
# libcompiler_builtins-*.rlib（报 "unknown cpu architecture: 65794"），且 zig 会把缓存
# 写进系统临时目录、在受限沙箱（DSH workspace-write）下直接 AccessDenied。
#
# rust-lld 与当前 rustc 同源，能正确解析这些 rlib；musl std 自带 self-contained
# crt1.o/crti.o/libc.a/libunwind.a，所以不需要外部 sysroot。实测产出真静态 ELF
# （无 PT_INTERP / PT_DYNAMIC），见 README「交叉编译」节。
#
# rust-lld 在 macOS 上是动态链接 libLLVM.dylib 的，需要 DYLD_FALLBACK_LIBRARY_PATH。
SYSROOT=$(rustc --print sysroot 2>/dev/null) || die "无法获得 rustc sysroot"
HOST_TRIPLE=$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')
LLD="$SYSROOT/lib/rustlib/$HOST_TRIPLE/bin/rust-lld"
[ -x "$LLD" ] || die "找不到 rust-lld: ${LLD}（rust 组件 rust-std 未装全？）"
export DYLD_FALLBACK_LIBRARY_PATH="$SYSROOT/lib${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"

# ── cargo build ──
# 全程不需要 zig：无 C 源（legacy/*.c 仅存档参考，不进构建），strip 由 rustc 的
# -C strip=symbols 完成。
mkdir -p build
export CARGO_TARGET_DIR="$ROOT/target"
export RUSTFLAGS="-C linker=$LLD -C linker-flavor=ld.lld -C strip=symbols"

info "cargo build --release --offline --target $TARGET"

if ! cargo build --release --offline --target "$TARGET" 2>&1; then
  die "cargo build 失败"
fi

BIN_REL="target/$TARGET/release/thermald"
[ -f "$BIN_REL" ] || die "cargo build 结束但未产出 $BIN_REL"

# strip 已由 RUSTFLAGS 的 -C strip=symbols 完成，此处不再二次处理。

# ── 拷贝到 build/thermald ──
OUT="build/thermald"
cp -f "$BIN_REL" "$OUT"
chmod 0755 "$OUT"

# ── 体积 & sha256 ──
SIZE=$(stat -f%z "$OUT" 2>/dev/null || stat -c%s "$OUT")
SHA=$(shasum -a 256 "$OUT" | awk '{print $1}')
printf '%s  %s\n' "$SHA" "$OUT" > build/thermald.sha256

echo
echo "── 产物 ─────────────────────────────────────"
echo "路径  : $OUT"
echo "体积  : $SIZE bytes"
echo "sha256: $SHA"
echo "目标  : ${TARGET}（静态）"
echo "──────────────────────────────────────────────"
file "$OUT"
