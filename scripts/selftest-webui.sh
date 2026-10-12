#!/usr/bin/env bash
# C 组自检：webroot 静态资源（零外链 / 无构建垃圾 / JS 语法 / 契约字段 / 体积统计）
# 用法：bash scripts/selftest-webui.sh   退出码 0=全过，非 0=有失败项
set -e

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WEB="$ROOT/webroot"
CONTRACT="$ROOT/CONTRACT.md"
FAIL=0

ok()  { printf '  [OK]   %s\n' "$*"; }
bad() { printf '  [FAIL] %s\n' "$*"; FAIL=1; }
info() { printf '  [INFO] %s\n' "$*"; }

printf '== webroot 自检 ==\n根目录: %s\n' "$ROOT"

# ---------- 1. 必要文件存在 ----------
printf '\n-- 1. 必要文件 --\n'
for f in index.html app.js style.css; do
  if [ -f "$WEB/$f" ]; then
    ok "$f 存在（$(wc -c < "$WEB/$f" | tr -d ' ') 字节）"
  else
    bad "$f 缺失"
  fi
done
[ -s "$WEB/index.html" ] || bad "index.html 为空"

# ---------- 2. 零外链 ----------
printf '\n-- 2. 外链检查（零 CDN / 零 http(s)） --\n'
HITS="$(grep -rInE 'https?://|//cdn|(src|href)[[:space:]]*=[[:space:]]*["'"'"']//|url\([[:space:]]*["'"'"']?//|@import[[:space:]]+url\([[:space:]]*["'"'"']?//' "$WEB" || true)"
if [ -n "$HITS" ]; then
  bad "发现外链引用："
  printf '%s\n' "$HITS" | sed 's/^/         /'
else
  ok "无 http:// / https:// / 协议相对 // 外链"
fi

# ---------- 3. 构建垃圾 ----------
printf '\n-- 3. 构建垃圾检查 --\n'
JUNK="$(find "$WEB" \( -name '*.map' -o -name '*.ts' -o -name '*.tsx' -o -name '*.scss' -o -name '*.less' \
  -o -name '*.orig' -o -name '*.rej' -o -name '*.bak' -o -name '*.tmp' -o -name '.DS_Store' \
  -o -name 'test*' -o -name '*spec*' -o -name '*selftest*' \
  -o -name 'node_modules' -o -name 'package.json' -o -name 'package-lock.json' \) -print)"
if [ -n "$JUNK" ]; then
  bad "发现构建垃圾 / 测试残留："
  printf '%s\n' "$JUNK" | sed 's/^/         /'
else
  ok "无 .map / 源文件 / node_modules / 测试残留"
fi
# 白名单：webroot 只允许三个交付文件（测试脚本须放 scripts/）
EXTRA="$(find "$WEB" -type f ! -name 'index.html' ! -name 'app.js' ! -name 'style.css' -print)"
if [ -n "$EXTRA" ]; then
  bad "webroot 只允许 index.html + app.js + style.css，发现多余文件："
  printf '%s\n' "$EXTRA" | sed 's/^/         /'
else
  ok "webroot 仅含 index.html / app.js / style.css"
fi

# ---------- 4. JS 语法 ----------
printf '\n-- 4. JS 语法 --\n'
if [ -f "$WEB/app.js" ]; then
  if command -v node >/dev/null 2>&1; then
    if node --check "$WEB/app.js"; then
      ok "node --check app.js 通过（$(node -v)）"
    else
      bad "node --check app.js 失败"
    fi
  else
    info "未找到 node，跳过 JS 语法检查"
  fi
fi

# ---------- 5. 契约字段引用（从 CONTRACT.md §4 抽取） ----------
printf '\n-- 5. 契约字段引用（CONTRACT.md §4） --\n'
if [ ! -f "$CONTRACT" ]; then
  bad "CONTRACT.md 缺失，无法校验字段"
elif [ ! -f "$WEB/app.js" ]; then
  bad "app.js 缺失，无法校验字段"
else
  # §4.1 status 与 §4.7 history 的 JSON 键（WebUI 实际消费的两块）
  sec41="$(awk '/^### 4\.1/{f=1;next} /^### 4\.2/{f=0} f' "$CONTRACT")"
  sec47="$(awk '/^### 4\.7/{f=1;next} /^### 4\.8/{f=0} f' "$CONTRACT")"
  keys="$(printf '%s\n%s\n' "$sec41" "$sec47" \
    | grep -oE '"[a-z][a-z0-9_]*"[[:space:]]*:' \
    | sed -E 's/^"//; s/"[[:space:]]*:$//' \
    | sort -u)"
  # §4.3 set / §4.5 reset 的应答字段
  for extra in changed error; do
    printf '%s\n' "$keys" | grep -qx "$extra" || keys="$(printf '%s\n%s\n' "$keys" "$extra")"
  done
  n=0; miss=0
  for k in $keys; do
    n=$((n + 1))
    if grep -qE "\\b${k}\\b" "$WEB/app.js"; then :; else
      bad "app.js 未引用契约字段: $k"
      miss=$((miss + 1))
    fi
  done
  [ "$miss" -eq 0 ] && ok "已引用 §4 全部 $n 个字段"
  # 小类 1 点名的必查字段
  for k in ok temp current target hardware_limit points; do
    grep -qE "\\b${k}\\b" "$WEB/app.js" || bad "缺少必查字段引用: $k"
  done
fi

# ---------- 5b. CLI JSON 契约字段检查器（自带正反样例，自证检查器本身有效） ----------
printf '\n-- 5b. CLI JSON 契约字段检查器（§4.1 status） --\n'
SAMPLE="$(mktemp -d)"
trap 'rm -rf "$SAMPLE"' EXIT

cat > "$SAMPLE/check.cjs" <<'CHKEOF'
const fs = require('fs');
const REQ = ['ok','version','running','pid','uptime_s','ts','temp','current','target',
  'intervention_temp','overtemp_temp','setpoint','hardware_limit','charging','state',
  'observer_z1','observer_z2','filtered_setpoint','applied_current','actual_current','config','history_points'];
const raw = fs.readFileSync(process.argv[2], 'utf8');
let doc, bad = 0;
try { doc = JSON.parse(raw); } catch (e) { console.log('FAIL  样例不是合法 JSON：' + e.message); process.exit(1); }
const miss = REQ.filter(k => !(k in doc));
if (miss.length) { console.log('FAIL  status 缺必需字段：' + miss.join(', ')); bad = 1; }
else console.log('OK    status 必需字段齐全（' + REQ.length + ' 项）');
if (doc.state !== undefined && !['idle','monitoring','controlling','stopped'].includes(doc.state)) {
  console.log('FAIL  state 非法取值：' + doc.state); bad = 1;
}
if (doc.running === false && doc.state !== 'stopped') {
  console.log('FAIL  running:false 时 state 必须为 stopped（契约 §4.1）'); bad = 1;
}
if (/:\s*(NaN|-?Infinity)\b/.test(raw)) { console.log('FAIL  JSON 含 NaN/inf（契约 §4 禁止，哨兵用 null）'); bad = 1; }
if (doc.config !== undefined && doc.config !== null && typeof doc.config !== 'object') {
  console.log('FAIL  config 必须是对象（§4.1 全字段）'); bad = 1;
}
process.exit(bad ? 1 : 0);
CHKEOF

cat > "$SAMPLE/good.json" <<'JSONEOF'
{"ok":true,"version":"0.1.0","running":true,"pid":12345,"uptime_s":3600,"ts":1791700000,
"temp":46.83,"current":20.76,"target":47.0,"intervention_temp":45.0,"overtemp_temp":47.3,
"setpoint":47.0,"hardware_limit":48.0,"charging":true,"state":"controlling",
"observer_z1":46.80,"observer_z2":-0.1205,"filtered_setpoint":47.0,"applied_current":20.76,
"actual_current":18.92,
"config":{"version":1,"setpoint":47.0,"hardware_limit":48.0},"history_points":512}
JSONEOF

cat > "$SAMPLE/bad.json" <<'JSONEOF'
{"ok":true,"version":"0.1.0","running":false,"pid":0,"uptime_s":0,"ts":0,"current":null,
"target":47.0,"intervention_temp":45.0,"overtemp_temp":47.3,"setpoint":47.0,
"charging":false,"state":"controlling","observer_z1":null,"observer_z2":null,
"filtered_setpoint":47.0,"applied_current":null,"actual_current":null,"history_points":0}
JSONEOF

if command -v node >/dev/null 2>&1; then
  if node "$SAMPLE/check.cjs" "$SAMPLE/good.json" > "$SAMPLE/good.log" 2>&1; then
    ok "检查器放行合规样例（检查器有效）"; sed 's/^/         /' "$SAMPLE/good.log"
  else
    bad "合规样例竟未通过——检查器有 bug"; sed 's/^/         /' "$SAMPLE/good.log"
  fi
  if node "$SAMPLE/check.cjs" "$SAMPLE/bad.json" > "$SAMPLE/bad.log" 2>&1; then
    bad "缺字段样例竟通过——检查器无效"
  else
    ok "检查器拦下缺字段样例（检查器有效）"; sed 's/^/         /' "$SAMPLE/bad.log"
  fi
  # 交叉验证：脚本内 REQ 清单必须与 app.js 的 STATUS_REQUIRED 完全一致（防漂移）
  if [ -f "$WEB/app.js" ]; then
    APPKEYS="$(sed -n '/var STATUS_REQUIRED = \[/,/\];/p' "$WEB/app.js" | grep -oE "'[a-z0-9_]+'" | tr -d "'" | sort | tr '\n' ' ')"
    CHKKEYS="$(sed -n "/^const REQ = \[/,/\];/p" "$SAMPLE/check.cjs" | grep -oE "'[a-z0-9_]+'" | tr -d "'" | sort | tr '\n' ' ')"
    if [ -n "$APPKEYS" ] && [ "$APPKEYS" = "$CHKKEYS" ]; then
      ok "app.js 的 STATUS_REQUIRED 与本脚本 REQ 清单一致（$(printf '%s' "$CHKKEYS" | wc -w | tr -d ' ') 项）"
    else
      bad "字段清单漂移：app.js=[$APPKEYS] 脚本=[$CHKKEYS]"
    fi
  fi
else
  info "未找到 node，跳过 JSON 契约检查器自证"
fi

# ---------- 6. 体积统计 ----------
printf '\n-- 6. webroot 统计 --\n'
NFILES="$(find "$WEB" -type f | wc -l | tr -d ' ')"
SIZE="$(du -sh "$WEB" | awk '{print $1}')"
BYTES="$(du -sk "$WEB" | awk '{print $1}')"
info "文件数: $NFILES"
info "总大小: $SIZE (${BYTES} KiB)"
find "$WEB" -type f -exec ls -l {} \; | awk '{printf "         %8d  %s\n", $5, $9}'

# ---------- 结果 ----------
printf '\n== 结果 ==\n'
if [ "$FAIL" -eq 0 ]; then
  printf 'PASS：全部检查通过（文件 %s 个，%s）\n' "$NFILES" "$SIZE"
  exit 0
else
  printf 'FAIL：存在未通过项\n'
  exit 1
fi
