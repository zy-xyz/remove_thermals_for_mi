/* 小米温控调节 WebUI —— 纯静态、零依赖、零外链。
   有 ksu.exec 走真机，否则演示模式。 */
(function (root) {
  'use strict';

  var POLL_MS = 2000;          // 轮询周期，契约要求 ≤2s
  var PAD = { l: 44, r: 44, t: 14, b: 24 };
  var STYLE = {
    temp: { line: '#ff7043', fill: 'rgba(255,112,67,0.13)' },
    current: { line: '#4fc3f7', fill: 'rgba(79,195,247,0.13)' },
    actual: { line: '#81c784', fill: 'rgba(129,199,132,0.10)' }
  };
  var STORE = 'mi_thermald_ui';

  /* ---------- §3 字段表（标签/单位/范围/人话解释），顺序即表单顺序 ---------- */
  var SPECS = [
    ['setpoint', '目标温度', '℃', 30, 47.5, '闭环想把电池稳在这个温度。调高=更热但充电更快。'],
    ['intervention_delta', '介入温度差', '℃', 0, 10, '设定点 − 该值 = 开始限流。调大=更晚介入、更凉。'],
    ['overtemp_delta', '超温余量', '℃', 0, 5, '目标之上的告警余量；超温判定线 = 目标 + 该值。'],
    ['hardware_limit', '硬件保护温度', '℃', 40, 60, '硬件级保护线，本模块不实施保护，只用来显示余量。'],
    ['current_min', '电流下限', 'A', 0, 5, '限流时允许压到的最小电流。'],
    ['current_max', '电流上限', 'A', 1, 30, '限流时允许放到的最大电流，也是满流上限。'],
    ['ambient', '环境温度', '℃', 0, 45, '模型假设的环境温度，影响预测的热平衡点。'],
    ['sensor_tau', '传感器热耦合滞后', 's', 0, 60, '传感器读数相对真实电芯温度的滞后；调大=读数更迟钝。'],
    ['k', '稳态增益', '℃/A', 0.01, 10, '每 1A 电流最终抬升的温升；标定不准会稳态偏差。'],
    ['t', '热时间常数', 's', 10, 2000, '发热/散热的快慢；越大温度变化越慢。'],
    ['tau', '纯滞后', 's', 0, 300, '施加电流到温度响应之间的死时间。'],
    ['wo', '观测器带宽', 'rad/s', 0.0001, 1, '越大跟踪越快，但抗噪声越差。'],
    ['wc', '控制器带宽', 'rad/s', 0.0001, 1, '越大响应越快，但更容易振荡和超调。'],
    ['lambda', 'IMC 滤波时间常数', 's', 0.05, 60, '闭环滤波；越大越平滑越慢，抗模型误差越强。'],
    ['b0', '输入增益标称值', '℃/(A·s)', 1e-6, 1, '控制器对“电流能产生多快温升”的标称估计；偏差大则响应变差。'],
    ['csv_max_points', '历史保留点数', '点', 60, 10000, 'history.csv 环形保留的最大点数，也是曲线窗口长度。'],
    ['sample_period', '采样周期', 's', 1, 10, '守护进程多久采一次样、写一次状态。']
  ];
  var INT_KEYS = { csv_max_points: 1, t: 1, tau: 1 };

  var KEYS = SPECS.map(function (s) { return s[0]; });
  var SPEC = {};
  SPECS.forEach(function (s) { SPEC[s[0]] = { label: s[1], unit: s[2], min: s[3], max: s[4], desc: s[5] }; });

  /* ---------- 纯函数：校验 ---------- */
  function validate(cfg) {
    var errors = {};
    SPECS.forEach(function (s) {
      var k = s[0], spec = SPEC[k], raw = cfg[k];
      if (raw === undefined || raw === null || raw === '') { errors[k] = '不能为空'; return; }
      var n = Number(raw);
      if (typeof raw === 'boolean' || (typeof raw === 'string' && raw.trim() === '') || !isFinite(n)) {
        errors[k] = '必须是数字';
        return;
      }
      if (n < spec.min || n > spec.max) errors[k] = '超出范围 [' + spec.min + ', ' + spec.max + ']';
    });
    if (!errors.current_min && !errors.current_max) {
      if (Number(cfg.current_max) < Number(cfg.current_min)) errors.current_max = '电流上限不能小于下限';
    }
    return errors;
  }

  /* ---------- 纯函数：拼 set 命令 ---------- */
  function buildSetCmd(pairs) {
    return 'thermald set ' + pairs.map(function (p) {
      return p[0] + '=' + (INT_KEYS[p[0]] ? String(Math.round(p[1])) : String(p[1]));
    }).join(' ');
  }

  /* ---------- 纯函数：容错解析 exec 输出 ---------- */
  function extractJSON(text) {
    if (text && typeof text === 'object') return text;
    if (typeof text !== 'string') throw new Error('返回内容不是文本');
    var s = text.indexOf('{');
    if (s < 0) throw new Error('未找到 JSON 对象');
    var depth = 0, inStr = false, esc = false;
    for (var i = s; i < text.length; i++) {
      var c = text[i];
      if (inStr) {
        if (esc) esc = false;
        else if (c === '\\') esc = true;
        else if (c === '"') inStr = false;
        continue;
      }
      if (c === '"') inStr = true;
      else if (c === '{') depth++;
      else if (c === '}') { depth--; if (depth === 0) return JSON.parse(text.slice(s, i + 1)); }
    }
    throw new Error('JSON 不完整');
  }

  function formatDuration(sec) {
    if (typeof sec !== 'number' || !isFinite(sec) || sec < 0) return '--';
    var s = Math.floor(sec), h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60);
    return (h > 0 ? h + ' 时 ' : '') + m + ' 分 ' + (s % 60) + ' 秒';
  }

  function judgeProtection(temp, limit) {
    if (typeof temp !== 'number' || !isFinite(temp)) return { text: '无数据', cls: '', margin: null };
    var margin = limit - temp;
    if (margin <= 0) return { text: '已超硬件线', cls: 'bad', margin: margin };
    if (margin <= 0.5) return { text: '逼近硬件线', cls: 'warn', margin: margin };
    return { text: '安全', cls: 'ok', margin: margin };
  }

  /* ---------- 纯函数：数据 → 像素 ---------- */
  function makeScale(points, W, H) {
    if (!points || !points.length) return null;
    var tmin = Infinity, tmax = -Infinity, cmax = 0;
    points.forEach(function (p) {
      if (typeof p.temp === 'number' && isFinite(p.temp)) {
        if (p.temp < tmin) tmin = p.temp;
        if (p.temp > tmax) tmax = p.temp;
      }
      if (typeof p.current === 'number' && isFinite(p.current) && p.current > cmax) cmax = p.current;
      // 实测电流也参与电流轴上界，否则这条线会越过画布顶。
      if (typeof p.actual === 'number' && isFinite(p.actual) && p.actual > cmax) cmax = p.actual;
    });
    if (!isFinite(tmin) || !isFinite(tmax)) return null;
    tmin = Math.floor(tmin - 1);
    tmax = Math.ceil(tmax + 1);
    if (tmax - tmin < 2) tmax = tmin + 2;
    return {
      W: W, H: H, tmin: tmin, tmax: tmax, cmax: Math.max(1, Math.ceil(cmax)),
      px: PAD.l, py: PAD.t, pw: W - PAD.l - PAD.r, ph: H - PAD.t - PAD.b
    };
  }
  function tempToY(v, s) { return s.py + (s.tmax - v) / (s.tmax - s.tmin) * s.ph; }
  function currentToY(v, s) { return s.py + (s.cmax - v) / s.cmax * s.ph; }
  function timeToX(t, s, pts) {
    var t0 = pts[0].t, t1 = pts[pts.length - 1].t;
    if (!(t1 > t0)) return s.px + s.pw / 2;
    return s.px + (t - t0) / (t1 - t0) * s.pw;
  }

  function mkSample(n) {
    var out = [], t0 = 1791700000;
    for (var i = 0; i < n; i++) {
      var u = i < 40 ? 24 : 12 + 9 * Math.cos(i / 13);
      out.push({
        t: t0 + i,
        temp: 46.0 + 1.6 * Math.sin(i / 11),
        current: u,
        actual: u * 0.94,          // 实测略低于设定，模拟线路损耗
        target: 47.0,
        state: i < 40 ? 'monitoring' : 'controlling'
      });
    }
    return out;
  }

  /* ---------- 环境 ---------- */
  var hasKsu = typeof root.ksu !== 'undefined' && root.ksu && typeof root.ksu.exec === 'function';
  var demo = !hasKsu;
  var demoCfg = null, demoPoints = null, demoUptime = 0;

  // ksu.exec 走 root shell（cwd=/），裸命令不在 PATH：必须绝对路径。
  // 模块目录名未必等于 module.prop 的 id，故用 KSU 提供的 moduleDir。
  var BIN = 'thermald';
  try {
    if (hasKsu && typeof root.ksu.moduleInfo === 'function') {
      var mi = root.ksu.moduleInfo();
      if (typeof mi === 'string') mi = JSON.parse(mi);
      if (mi && mi.moduleDir) BIN = mi.moduleDir + '/bin/thermald';
    }
  } catch (e) { /* 取不到就退回裸命令；演示模式下无妨 */ }
  function withBin(cmd) { return String(cmd).replace(/^thermald\b/, BIN); }

  function exec(cmd) {
    if (!demo) return Promise.resolve(root.ksu.exec(withBin(cmd))).then(normalizeExec);
    return demoExec(cmd);
  }
  // KernelSU 不同版本可能 resolve 字符串 / {stdout} / {errno,stdout}
  function normalizeExec(res) {
    if (res && typeof res === 'object' && typeof res.stdout === 'string') return res.stdout;
    return res;
  }
  function demoExec(cmd) {
    return new Promise(function (resolve, reject) {
      root.setTimeout(function () {
        if (/^thermald\s+status\b/.test(cmd)) {
          if (!demoPoison) return resolve(demoStatus());
          return resolve(demoPoison); // 由测试注入的坏数据（模拟设备/契约异常）
        }
        if (/^thermald\s+history\b/.test(cmd)) {
          if (!demoPoints) demoPoints = mkSample(120);
          var uu = 12 + 9 * Math.cos(demoPoints.length / 13);
          demoPoints.push({
            t: demoPoints[demoPoints.length - 1].t + 1,
            temp: 46.0 + 1.6 * Math.sin(demoPoints.length / 11),
            current: uu,
            actual: uu * 0.94,
            target: 47.0, state: 'controlling'
          });
          if (demoPoints.length > 600) demoPoints.shift();
          return resolve(JSON.stringify({ ok: true, count: demoPoints.length, points: demoPoints.slice(-600) }));
        }
        if (/^thermald\s+set\s+--reset/.test(cmd)) {
          demoCfg = JSON.parse(JSON.stringify(DEFAULTS));
          return resolve(JSON.stringify({ ok: true, changed: {}, config: demoCfg }));
        }
        if (/^thermald\s+set\b/.test(cmd)) {
          if (!demoCfg) demoCfg = JSON.parse(JSON.stringify(DEFAULTS));
          var changed = {}, bad = null;
          cmd.replace(/^thermald\s+set\s+/, '').trim().split(/\s+/).forEach(function (kv) {
            var i = kv.indexOf('='), k = kv.slice(0, i), v = Number(kv.slice(i + 1));
            if (!SPEC[k]) { bad = '未知参数: ' + k; return; }
            if (!isFinite(v) || v < SPEC[k].min || v > SPEC[k].max) {
              bad = k + ' 超出范围 [' + SPEC[k].min + ', ' + SPEC[k].max + ']: ' + kv.slice(i + 1); return;
            }
            changed[k] = v; demoCfg[k] = v;
          });
          if (bad) return resolve(JSON.stringify({ ok: false, error: bad }));
          return resolve(JSON.stringify({ ok: true, changed: changed, config: demoCfg }));
        }
        reject(new Error('演示模式不支持的命令: ' + cmd));
      }, 5);
    });
  }
  var demoPoison = null;

  var DEFAULTS = {
    version: 1, setpoint: 47.0, intervention_delta: 2.0, overtemp_delta: 0.3,
    hardware_limit: 48.0, current_min: 0.0, current_max: 24.0, ambient: 25.0,
    sensor_tau: 8.0, k: 1.05, t: 180.0, tau: 25.0, wo: 0.04305555555555556,
    wc: 0.04305555555555556, lambda: 1.0, b0: 0.005833333333333334,
    csv_max_points: 600, sample_period: 1.0
  };

  function demoStatus() {
    var t = 46.0 + 1.6 * Math.sin((demoPoints ? demoPoints.length : 0) / 11);
    demoUptime += POLL_MS / 1000;
    var cfg = demoCfg || JSON.parse(JSON.stringify(DEFAULTS));
    return JSON.stringify({
      ok: true, version: '0.1.0', running: true, pid: 12345,
      uptime_s: Math.round(demoUptime), ts: Math.floor(Date.now() / 1000),
      temp: t, current: 12 + 9 * Math.cos((demoPoints ? demoPoints.length : 0) / 13),
      target: cfg.setpoint, intervention_temp: cfg.setpoint - cfg.intervention_delta,
      overtemp_temp: cfg.setpoint + cfg.overtemp_delta, setpoint: cfg.setpoint,
      hardware_limit: cfg.hardware_limit, charging: true, state: 'controlling',
      observer_z1: t, observer_z2: -0.12, filtered_setpoint: cfg.setpoint,
      applied_current: 20, actual_current: 18.8, config: cfg,
      history_points: demoPoints ? demoPoints.length : 0
    });
  }

  /* ---------- #4.1 status 必需字段（与 selftest 共用同一份清单） ---------- */
  var STATUS_REQUIRED = [
    'ok', 'version', 'running', 'pid', 'uptime_s', 'ts', 'temp', 'current', 'target',
    'intervention_temp', 'overtemp_temp', 'setpoint', 'hardware_limit', 'charging',
    'state', 'observer_z1', 'observer_z2', 'filtered_setpoint', 'applied_current',
    'actual_current', 'config', 'history_points'
  ];
  function checkStatus(d) {
    if (d.ok === false) throw new Error(d.error || '设备返回失败');
    missingFields = STATUS_REQUIRED.filter(function (k) { return !(k in d); });
    return d;
  }
  var missingFields = [], contractWarned = false; // 「契约缺字段」提示只弹一次

  /* ---------- UI ---------- */
  var el = {};
  function $(id) { return document.getElementById(id); }
  function setMsg(node, text, cls) {
    if (!node) return;
    node.textContent = text || '';
    node.className = 'msg ' + (cls || '') + (text ? '' : ' hidden');
  }
  function setCards(f, where) {
    var p = judgeProtection(f.temp, f.hardware_limit);
    el.temp.textContent = f.temp === null || f.temp === undefined ? '--' : f.temp.toFixed(2) + ' ℃';
    el.tempSub.textContent = f.state_text;
    el.cur.textContent = f.current === null || f.current === undefined ? '--' : f.current.toFixed(2) + ' A';
    el.curSub.textContent = f.charging ? '充电中' : '未充电（不参与控制）';
    el.act.textContent = f.actual === null || f.actual === undefined ? '--' : f.actual.toFixed(2) + ' A';
    el.actSub.textContent = f.charging ? '充电中' : '未充电';
    el.target.textContent = f.target.toFixed(2) + ' ℃';
    el.targetSub.textContent = '介入 ' + f.intervention_temp.toFixed(1) + ' ℃ / 超温 ' + f.overtemp_temp.toFixed(1) + ' ℃';
    el.prot.textContent = p.text;
    el.prot.className = 'val ' + p.cls;
    el.protSub.textContent = p.margin === null ? '--' : '距硬件线 ' + p.margin.toFixed(2) + ' ℃' + (where || '');
    el.uptime.textContent = formatDuration(f.uptime_s);
    el.uptimeSub.textContent = 'PID ' + (f.pid || '--');
  }

  var STATE_TEXT = { idle: '空载', monitoring: '监控中', controlling: '闭环限流中', stopped: '守护进程未运行' };

  function statusToFields(d) {
    var c = d.config || {};
    return {
      running: d.running, pid: d.pid, uptime_s: d.uptime_s,
      temp: typeof d.temp === 'number' ? d.temp : null,
      current: typeof d.current === 'number' ? d.current : (typeof d.applied_current === 'number' ? d.applied_current : null),
      actual: typeof d.actual_current === 'number' ? d.actual_current : null,
      target: num(c.setpoint, d.target), intervention_temp: num(d.intervention_temp, 0),
      overtemp_temp: num(d.overtemp_temp, 0),
      hardware_limit: num(c.hardware_limit, d.hardware_limit), charging: !!d.charging,
      state_text: STATE_TEXT[d.state] || String(d.state || '未知')
    };
  }
  function num(a, b) { return typeof a === 'number' && isFinite(a) ? a : (typeof b === 'number' && isFinite(b) ? b : 0); }

  /* ---------- 曲线 ---------- */
  var chartState = { points: [], msg: '', rects: [] };
  function drawChart(points, msg) {
    var cv = el.chart;
    if (!cv || !cv.getContext) return;
    var ctx = cv.getContext('2d');
    var W = cv.width, H = cv.height;
    ctx.clearRect(0, 0, W, H);
    chartState.points = points || [];
    chartState.msg = msg || '';
    chartState.rects = [];
    if (!chartState.points.length) {
      if (el.chartMsg) { el.chartMsg.textContent = msg || '暂无历史数据'; el.chartMsg.className = 'chart-msg'; }
      return;
    }
    if (el.chartMsg) { el.chartMsg.textContent = ''; el.chartMsg.className = 'chart-msg hidden'; }
    var s = makeScale(chartState.points, W, H);
    if (!s) { el.chartMsg.textContent = '历史数据缺少数值字段，无法绘图'; return; }

    /* 网格 + 双轴刻度 */
    ctx.strokeStyle = '#2a3138'; ctx.fillStyle = '#8b949e';
    ctx.font = '11px sans-serif'; ctx.lineWidth = 1;
    for (var i = 0; i <= 4; i++) {
      var y = Math.round(s.py + s.ph * i / 4) + 0.5;
      ctx.beginPath(); ctx.moveTo(s.px, y); ctx.lineTo(s.px + s.pw, y); ctx.stroke();
      ctx.textAlign = 'right';
      ctx.fillText((s.tmax - (s.tmax - s.tmin) * i / 4).toFixed(1), s.px - 6, y + 4);
      ctx.textAlign = 'left';
      ctx.fillText((s.cmax * (4 - i) / 4).toFixed(1), s.px + s.pw + 6, y + 4);
    }
    ctx.textAlign = 'left'; ctx.fillText('℃', 4, s.py + 10);
    ctx.textAlign = 'right'; ctx.fillText('A', W - 4, s.py + 10);

    /* 目标温度虚线 */
    var tg = chartState.points[chartState.points.length - 1].target;
    if (typeof tg === 'number' && isFinite(tg) && tg >= s.tmin && tg <= s.tmax) {
      var gy = Math.round(tempToY(tg, s)) + 0.5;
      ctx.save(); ctx.setLineDash([4, 4]); ctx.strokeStyle = '#5a6b7a';
      ctx.beginPath(); ctx.moveTo(s.px, gy); ctx.lineTo(s.px + s.pw, gy); ctx.stroke(); ctx.restore();
    }

    /* 折线：设定电流（细）→ 实测电流（细）→ 温度（主） */
    series(ctx, chartState.points, s, 'current', currentToY, STYLE.current);
    series(ctx, chartState.points, s, 'actual', currentToY, STYLE.actual);
    series(ctx, chartState.points, s, 'temp', tempToY, STYLE.temp);
    if (el.chartMeta) {
      el.chartMeta.textContent = '点数 ' + chartState.points.length +
        ' · 温度轴 ' + s.tmin + '~' + s.tmax + ' ℃ · 电流轴 0~' + s.cmax + ' A' +
        ' · 窗口 ' + formatDuration(chartState.points[chartState.points.length - 1].t - chartState.points[0].t);
    }
  }
  function series(ctx, pts, s, key, toY, st) {
    var i, p, seg = [];
    for (i = 0; i < pts.length; i++) {
      p = pts[i];
      if (typeof p[key] !== 'number' || !isFinite(p[key])) { if (seg.length) { strokeSeg(ctx, seg, s, st); seg = []; } continue; }
      seg.push({ x: timeToX(p.t, s, pts), y: toY(p[key], s) });
    }
    if (seg.length) strokeSeg(ctx, seg, s, st);
  }
  function strokeSeg(ctx, seg, s, st) {
    var i;
    if (seg.length > 1) {
      ctx.beginPath();
      for (i = 0; i < seg.length; i++) { i ? ctx.lineTo(seg[i].x, seg[i].y) : ctx.moveTo(seg[i].x, seg[i].y); }
      ctx.lineTo(seg[seg.length - 1].x, s.py + s.ph);
      ctx.lineTo(seg[0].x, s.py + s.ph);
      ctx.closePath(); ctx.fillStyle = st.fill; ctx.fill();
    }
    ctx.beginPath();
    for (i = 0; i < seg.length; i++) { i ? ctx.lineTo(seg[i].x, seg[i].y) : ctx.moveTo(seg[i].x, seg[i].y); }
    ctx.strokeStyle = st.line; ctx.lineWidth = 2; ctx.stroke();
  }

  /* ---------- 参数表单 ---------- */
  function buildForm() {
    var host = el.form; if (!host) return;
    var html = '<div class="grid">';
    SPECS.forEach(function (sp) {
      var k = sp[0];
      html += '<div class="field" id="f-' + k + '">' +
        '<label for="i-' + k + '">' + sp[1] + '<span class="unit">' + sp[2] + '</span></label>' +
        '<input id="i-' + k + '" name="' + k + '" type="number" step="any" inputmode="decimal">' +
        '<div class="range">范围 ' + sp[3] + ' ~ ' + sp[4] + ' ' + sp[2] + '</div>' +
        '<div class="desc">' + sp[5] + '</div>' +
        '<div class="err" id="e-' + k + '"></div></div>';
    });
    host.innerHTML = html + '</div>';
    host.addEventListener('input', function () { markErrors(validate(readForm())); });
  }
  function markErrors(errs) {
    SPECS.forEach(function (sp) {
      var f = document.getElementById('f-' + sp[0]), e = document.getElementById('e-' + sp[0]);
      if (!f) return;
      if (errs[sp[0]]) { f.className = 'field invalid'; if (e) e.textContent = errs[sp[0]]; }
      else { f.className = 'field'; if (e) e.textContent = ''; }
    });
  }

  function readForm() {
    var out = {};
    KEYS.forEach(function (k) { var n = $('i-' + k); out[k] = n ? n.value : ''; });
    return out;
  }
  function writeForm(cfg) {
    KEYS.forEach(function (k) {
      var n = $('i-' + k);
      if (n && cfg[k] !== undefined && cfg[k] !== null) n.value = String(cfg[k]);
      var f = document.getElementById('f-' + k), e = document.getElementById('e-' + k);
      if (f) { f.className = 'field'; if (e) e.textContent = ''; }
    });
  }

  function save() {
    var v = readForm(), errs = validate(v), names = Object.keys(errs);
    if (names.length) {
      names.forEach(function (k) {
        var f = document.getElementById('f-' + k), e = document.getElementById('e-' + k);
        if (f) f.className = 'field invalid';
        if (e) e.textContent = errs[k];
      });
      setMsg(el.formMsg, '未发送请求，共 ' + names.length + ' 项不合法：' +
        names.map(function (k) { return SPEC[k].label + '——' + errs[k]; }).join('；'), 'err');
      return Promise.resolve({ blocked: true, errors: errs });
    }
    setMsg(el.formMsg, '保存中…', '');
    var pairs = KEYS.map(function (k) { return [k, v[k]]; });
    var cmd = buildSetCmd(pairs);
    return exec(cmd).then(function (res) {
      var d = extractJSON(res);
      if (d.ok === false) throw new Error(d.error || '设备拒绝保存');
      writeForm(d.config || {});
      setMsg(el.formMsg, '已保存：' + Object.keys(d.changed || {}).join('、'), 'ok');
      return d;
    }).catch(function (e) {
      setMsg(el.formMsg, '保存失败：' + (e && e.message ? e.message : String(e)), 'err');
      return { blocked: false, error: String(e && e.message || e) };
    });
  }

  function resetCfg() {
    if (root.confirm && !root.confirm('恢复出厂参数？当前值会被覆盖。')) return Promise.resolve();
    return exec('thermald set --reset').then(function (res) {
      var d = extractJSON(res);
      if (d.ok === false) throw new Error(d.error || '设备拒绝重置');
      writeForm(d.config || {});
      setMsg(el.formMsg, '已恢复出厂参数', 'ok');
      return d;
    }).catch(function (e) { setMsg(el.formMsg, '重置失败：' + (e && e.message ? e.message : String(e)), 'err'); });
  }

  function exportJSON() {
    var v = readForm();
    KEYS.forEach(function (k) { if (v[k] !== '' && !isNaN(Number(v[k]))) v[k] = INT_KEYS[k] ? Math.round(Number(v[k])) : Number(v[k]); });
    var text = JSON.stringify(v, null, 2);
    if (el.exportOut) { el.exportOut.textContent = text; el.exportOut.className = ''; }
    try {
      if (!root.Blob || !root.URL || !root.URL.createObjectURL) return text;
      var url = root.URL.createObjectURL(new root.Blob([text], { type: 'application/json' }));
      var a = document.createElement('a');
      a.href = url; a.download = 'thermald-config.json';
      if (a.click) a.click();
      root.setTimeout(function () { root.URL.revokeObjectURL(url); }, 3000);
    } catch (e) { /* 导出失败不影响页面 */ }
    return text;
  }

  /* ---------- 轮询 ---------- */
  var paused = false, tickTimer = null, lastCfg = null;
  function tick() {
    if (paused) return Promise.resolve();
    return exec('thermald status').then(function (res) {
      var d = checkStatus(extractJSON(res));
      if (missingFields.length && !contractWarned) {
        contractWarned = true;
        setMsg(el.banner, '设备返回数据缺少契约 §4.1 字段：' + missingFields.join('、') + '（部分卡片可能显示 --）', 'err');
      }
      if (!d.running) {
        setMsg(el.banner, '守护进程未运行（thermald 未启动），以下为配置文件默认值。', 'err');
      }
      var f = statusToFields(d);
      setCards(f, '');
      if (d.config) {
        lastCfg = d.config;
        // 表单未被手动改动时，用设备值刷新（保存后一个轮询周期内即生效）
        var editing = el.form && document.activeElement && el.form.contains && el.form.contains(document.activeElement);
        if (!editing) writeForm(d.config);
      }
      return exec('thermald history --limit 600');
    }).then(function (res) {
      if (res === undefined) return; // status 失败已提示
      var d = extractJSON(res);
      if (d.ok === false) throw new Error(d.error || '历史数据读取失败');
      drawChart(d.points || [], '暂无历史数据');
    }).catch(function (e) {
      var m = '设备数据读取失败：' + (e && e.message ? e.message : String(e)) + '（下个周期自动重试）';
      setCards({ temp: null, current: null, target: 0, intervention_temp: 0, overtemp_temp: 0, hardware_limit: 0, charging: false, uptime_s: NaN, pid: '--', state_text: '读取失败' }, '');
      drawChart(chartState.points, m);
      if (el.chartMsg) { el.chartMsg.textContent = m; el.chartMsg.className = 'chart-msg'; }
      if (el.chartMeta) el.chartMeta.textContent = m;
    });
  }

  function start() {
    el = {
      banner: $('banner'), temp: $('c-temp'), tempSub: $('c-temp-sub'), cur: $('c-cur'), curSub: $('c-cur-sub'),
      act: $('c-act'), actSub: $('c-act-sub'), target: $('c-target'), targetSub: $('c-target-sub'), prot: $('c-prot'), protSub: $('c-prot-sub'),
      uptime: $('c-uptime'), uptimeSub: $('c-uptime-sub'), chart: $('chart'), chartMsg: $('chart-msg'),
      chartMeta: $('chart-meta'), form: $('form-params'), formMsg: $('form-msg'), exportOut: $('export-out')
    };
    buildForm();
    writeForm(lastCfg || DEFAULTS);
    if (demo) { setMsg(el.banner, '未运行在 KernelSU 环境（演示模式）：数据为内置样例。', ''); el.banner.className = 'msg'; }
    var bp = $('btn-pause');
    if (bp) bp.addEventListener('click', function () {
      paused = !paused; bp.textContent = paused ? '继续' : '暂停';
      setMsg(el.formMsg, paused ? '已暂停轮询' : '', '');
    });
    var bs = $('btn-save'); if (bs) bs.addEventListener('click', save);
    var br = $('btn-reset'); if (br) br.addEventListener('click', resetCfg);
    var bl = $('btn-reload'); if (bl) bl.addEventListener('click', function () { tick(); });
    var be = $('btn-export'); if (be) be.addEventListener('click', exportJSON);
    tick();
    tickTimer = root.setInterval(function () { tick(); }, POLL_MS);
  }

  var API = {
    SPECS: SPECS, SPEC: SPEC, KEYS: KEYS, DEFAULTS: DEFAULTS, INT_KEYS: INT_KEYS,
    STATUS_REQUIRED: STATUS_REQUIRED, POLL_MS: POLL_MS,
    validate: validate, buildSetCmd: buildSetCmd, extractJSON: extractJSON,
    formatDuration: formatDuration, judgeProtection: judgeProtection,
    makeScale: makeScale, tempToY: tempToY, currentToY: currentToY, timeToX: timeToX,
    mkSample: mkSample, statusToFields: statusToFields, checkStatus: checkStatus,
    readForm: readForm, writeForm: writeForm, save: save, resetCfg: resetCfg, exportJSON: exportJSON,
    tick: tick, start: start, drawChart: drawChart,
    _setExec: function (fn) { exec = fn; },
    _state: function () { return { paused: paused, lastCfg: lastCfg, chart: chartState, demo: demo }; },
    _setDemo: function (v) { demo = v; },
    _setPoison: function (v) { demoPoison = v; }
  };
  root.__test = API;
  root.__mi_thermald_start = start;
  if (typeof module !== 'undefined' && module.exports) module.exports = API;
  if (typeof document !== 'undefined' && root.document) {
    if (root.document.readyState === 'loading') root.document.addEventListener('DOMContentLoaded', start);
    else start();
  }
})(typeof globalThis !== 'undefined' ? globalThis : this);
