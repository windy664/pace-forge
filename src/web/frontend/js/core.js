/* 基础工具、API 封装与登录。由 index.html 以 <script src="/js/core.js"> 引入，
 * 经 STATIC_ASSETS 静态路由下发。经典 script 按骨架里的引用顺序执行且共享
 * 全局词法作用域：后面文件的顶层语句直接引用这里的绑定（const/let 不提升），
 * 骨架里的引用顺序不能动。 */
const $ = id => document.getElementById(id);
/* 地图和 Canvas 上的颜色也要走令牌，但 Leaflet 是把 color 写进 SVG 的属性里，
   那里 var() 不生效，所以在运行期把令牌解析成真实色值再传进去。 */
const tok = n => getComputedStyle(document.documentElement).getPropertyValue(n).trim();
/* 验证过的控制台口令保存在当前标签页，并通过 X-Pace-Token 请求头传递。 */
const CONSOLE_TOKEN_KEY = 'pace_console_token';
let consoleToken = '';
try {
  sessionStorage.removeItem('pf_token'); // 旧版曾直接保存网址中的口令
  consoleToken = sessionStorage.getItem(CONSOLE_TOKEN_KEY) || '';
} catch (_) {}
function rememberConsoleToken(token) {
  consoleToken = token;
  try {
    if (token) sessionStorage.setItem(CONSOLE_TOKEN_KEY, token);
    else sessionStorage.removeItem(CONSOLE_TOKEN_KEY);
  } catch (_) {}
}
const authHeaders = () => (consoleToken ? { 'X-Pace-Token': consoleToken } : {});
/* 单个请求的超时：上游挂起时 fetch 会一直挂着，按钮停在 busy 态无法恢复；
   到点主动 abort，抛出明确的超时文案，而不是无限等待。
   取值要盖住最慢的合法请求：批量跑/单场提交里包含路网与瓦片拉取，串行时
   可能几十秒才返回。定太短（60s）会把"服务端还在提交、前端已判超时"变成
   用户重试导致的重复提交。 */
const API_TIMEOUT_MS = 180000;
const api = async (url, opt) => {
  const ctrl = new AbortController();
  const timer = setTimeout(() => ctrl.abort(), API_TIMEOUT_MS);
  let r;
  try {
    r = await fetch(url, {
      ...(opt || {}),
      mode: 'same-origin',
      signal: ctrl.signal,
      headers: { ...authHeaders(), ...((opt && opt.headers) || {}) }
    });
  } catch (e) {
    clearTimeout(timer);
    if (e && e.name === 'AbortError') throw new Error('请求超时：后端长时间无响应，请刷新重试');
    throw new Error('网络请求失败：' + ((e && e.message) || e));
  }
  clearTimeout(timer);
  // 非 JSON 响应（网关错误页、空体、被代理截断）不能让 `r.json()` 抛
  // SyntaxError——那句 "Unexpected token u in JSON at position 0" 用户看不懂。
  // 把状态码和 content-type 带出来，至少能判断是「服务挂了」还是「路由错了」。
  const j = await r.json().catch(() => null);
  if (!j) {
    const error = new Error(`HTTP ${r.status}：响应不是 JSON（${r.headers.get('content-type') || '无 content-type'}）`);
    if (!r.ok) error.status = r.status;
    throw error;
  }
  if (!r.ok || !j.ok) {
    const error = new Error(j.error || '请求失败');
    error.status = r.status;
    error.kind = j.kind;
    throw error;
  }
  return j.data;
};
/* 按钮进入/退出「进行中」态：禁用 + 转圈，避免连点重复提交 */
const busy = (btn, on) => { if (btn) { btn.classList.toggle('busy', on); btn.disabled = on; } };

/* 构建号：从 /api/health 取真实版本号。
   原来这里写死 `9077a37 · 11-06 09:34`，早就和 HEAD 对不上了。
   纯静态预览（没起服务）时请求会失败，保持「本地控制台」不变。 */
(async () => {
  try {
    const r = await fetch('/api/health', { cache: 'no-store' });
    const j = await r.json();
    const v = j && (j.version || (j.data && j.data.version));
    if (!v) return;
    const text = `v${v} · 本地控制台`;
    ['buildTag', 'loginTag'].forEach(id => { const el = $(id); if (el) el.textContent = text; });
  } catch (_) { /* 无后端：留默认文案 */ }
})();

/* ---------- 登录 ---------- */
$('btn').onclick = async () => {
  const u = $('u').value.trim(), p = $('p').value;
  if (!u || !p) return setMsg('手机号和密码都要填', 'err');
  $('btn').disabled = true;
  setMsg('登录中…');
  try {
    const me = await api('/api/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: u, password: p })
    });
    $('p').value = '';
    enter(me);
  } catch (e) {
    setMsg(e.message, 'err');
  } finally {
    $('btn').disabled = false;
  }
};
$('p').onkeydown = e => { if (e.key === 'Enter') $('btn').click(); };
function setMsg(t, cls) { const m = $('msg'); m.textContent = t; m.className = 'msg ' + (cls || ''); }

/* ---------- 进入主界面 ---------- */
function enter(me) {
  $('login').style.display = 'none';
  $('app').style.display = 'block';
  $('ava').textContent = (me.name || me.username || '?').slice(0, 1);
  $('uname').textContent = me.name || me.username;
  $('uinfo').textContent = [me.campus_name, me.depart, me.grade_class].filter(Boolean).join(' · ') || '—';
  initMap();
  loadRecords();
  loadEgress();
}

/* 出口 IP 自检（#10）：亮出提交时服务端用的公网出口 IP，不伪造、不选路。
   IP 字符串来自外部接口，只走 textContent/title，不进 innerHTML。
   失败显示「出口未知」——它是自检项，不影响提交，也不弹错。 */
async function loadEgress() {
  const t = $('egressTag');
  if (!t) return;
  try {
    const j = await api('/api/egress-ip');
    if (j && j.ok) {
      t.textContent = '出口 ' + j.ip;
      t.title = '提交出口 IP（探测源 ' + j.source + '）。归属地和学校不在同城时，记录的 IP 特征会异常——同类工具成片被封的直接原因。';
    } else {
      t.textContent = '出口未知';
      t.title = '公网出口探测失败（不影响提交，可忽略；有代理或断外网时常见）';
    }
  } catch (_) {
    t.textContent = '出口未知';
    t.title = '公网出口探测失败（不影响提交，可忽略）';
  }
}
$('logout').onclick = async () => {
  // 退出登录 = 让服务端 token 失效 + 清本地登录态。
  // 账号和存盘密码都留着，下次打开页面点「重登」即可，不用重新输密码。
  const btn = $('logout');
  btn.disabled = true;
  btn.textContent = '退出中…';
  try {
    const r = await api('/api/logout', {
      method: 'POST',
      cache: 'no-store'
    });
    // 服务端可能没确认（网络/接口失败）。本地照清，但得让用户知道会话
    // 还活着——不说一声，半小时内重登就会撞 10121 锁一小时。
    if (!r.remoteLogout) {
      alert('本地已退出，但服务端没有确认（网络或接口失败）。\n半小时内别重登，否则可能被锁一小时。');
    }
  } catch (e) {
    alert('退出登录失败：' + e.message);
    btn.disabled = false;
    btn.textContent = '退出登录';
    return;
  }
  // 清掉本地残留，避免任何 form/地图状态带回来
  try { localStorage.clear(); sessionStorage.clear(); } catch (_) {}
  rememberConsoleToken(consoleToken);
  location.replace(location.origin + location.pathname);
};
$('reload').onclick = loadRecords;

$('rMode').onchange = () => {
  const v = $('rMode').value;
  $('laneWrap').style.display = v === 'stadium' ? '' : 'none';
  $('drawWrap').style.display = v === 'draw' ? '' : 'none';
  if (v === 'draw') {
    // 手绘必须看得见地图，滚过去并确保地图已初始化
    initMap();
    setTimeout(() => {
      initDrawing();
      $('map').scrollIntoView({ behavior: 'smooth', block: 'center' });
      if (map) map.invalidateSize();
    }, 60);
    setOut('已切到手绘：在地图上按住拖动画出路线，画完点「用这条轨迹」');
  } else {
    setDrawMode(false);
    if (v === 'stadium') {
      setOut('已切到操场绕圈：算法按道次生成标准跑道');
    } else {
      setOut('已切到自动规划：算法按围栏和点位算路线');
    }
  }
};

/* ---------- 时长自动生成 ----------
 * 每次都 12:00 整，时长一模一样，一眼就是脚本。改成按配速区间随机：
 *   duration(分) = 距离(km) × 配速(分/km)，配速在 [min, max] 里均匀取
 * 5.5~6.6 分/km 跑 2 km 就是 11:00~13:12，天然浮动。
 */
function paceRange() {
  const a = parseFloat($('rPaceMin').value);
  const b = parseFloat($('rPaceMax').value);
  const lo = Number.isFinite(a) && a >= 2 && a <= 12 ? a : 5.5;
  const hi = Number.isFinite(b) && b >= 2 && b <= 12 ? b : 6.6;
  return [Math.min(lo, hi), Math.max(lo, hi)];
}

function autoDur(dist) {
  const [lo, hi] = paceRange();
  const pace = lo + Math.random() * (hi - lo);
  // 落到 1 秒粒度，看起来就是真跑出来的
  return Math.max(3, Math.round(dist * pace * 60) / 60);
}

function fmtPaceMin(v) {
  if (!(v > 0)) return '—';
  const m = Math.floor(v), sec = Math.round((v - m) * 60);
  return m + "'" + String(sec).padStart(2, '0') + '"';
}

/// 取本次要用的时长（分钟）。勾了自动就现随机一个并回填输入框。
function readDur(dist) {
  if (!$('rAutoDur').checked) return parseFloat($('rDur').value) || 0;
  const d = autoDur(dist);
  $('rDur').value = d.toFixed(2);
  return d;
}

/// 海拔区间：两个都填、且上界大于下界 1m 以上才发。
/// 后端 parse_alt_range 会再校验一次，这里只是不把明显没意义的请求发出去。
function altBody() {
  const lo = parseFloat($('rAltMin').value);
  const hi = parseFloat($('rAltMax').value);
  if (!isFinite(lo) || !isFinite(hi) || hi <= lo + 1) return {};
  return { altMin: lo, altMax: hi };
}

function syncDurHint() {
  const auto = $('rAutoDur').checked;
  $('rDur').readOnly = auto;
  if (auto) {
    const dist = parseFloat($('rDist').value) || 2;
    const [lo, hi] = paceRange();
    $('durHint').textContent = `（${fmtPaceMin(lo)}~${fmtPaceMin(hi)}/km → ${(dist * lo).toFixed(1)}~${(dist * hi).toFixed(1)} 分）`;
  } else {
    $('durHint').textContent = '';
  }
}

['rAutoDur', 'rPaceMin', 'rPaceMax', 'rDist'].forEach(id => {
  const el = $(id);
  if (el) {
    el.oninput = syncDurHint;
    el.onchange = syncDurHint;
    // 改目标距离时，手绘面板那行「还差 X m」也要跟着变，
    // 不然用户画完再去调距离，看到的还是旧差值。
    el.addEventListener('input', () => { if (strokes.length) redrawStrokes(); });
  }
});
syncDurHint();

$('planBtn').onclick = async () => {
  let st = null;
  try { st = await resolveStartAt(); } catch (e) { if (e.message !== '已取消') setOut(e.message, true); return; }
  const dist = parseFloat($('rDist').value);
  const dur = readDur(dist);
  const stepsRaw = $('rSteps').value.trim();
  const steps = stepsRaw === '' ? null : parseInt(stepsRaw, 10);
  if (!(dist > 0) || !(dur > 0)) return setOut('距离和时长要填正数', true);

  const btn = $('planBtn');
  busy(btn, true);
  setOut('生成中…');
  try {
    const r = await api('/api/plan', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        distance: dist, duration: dur,
        steps: (Number.isFinite(steps) && steps > 0) ? steps : 0,
        mode: $('rMode').value, lane: parseInt($('rLane').value, 10) || 1,
        preferRoads: !$('rNoRoads').checked,
        sportType: parseInt($('rRunMode').value, 10) || 1,
        runMode: (parseInt($('rRunMode').value, 10) || 1) === 1 ? 1 : 2,
        startAtMs: (st ? st.startMs : undefined),
        ...locBody(),
        ...endBody(),
        customPath: $('rMode').value === 'draw'
          ? (() => { const m = useDrawing(); return m && m.length >= 2 ? m : undefined; })()
          : undefined
      })
    });
    setOut('');
    const modeTxt = r.mode === 'stadium' ? `操场第 ${r.lane} 道`
      : (r.mode === 'draw' ? '手绘轨迹' : '自由路线');
    const acTxt = r.stepAutocorr != null
      ? `${r.stepAutocorr.toFixed(3)} ${r.stepAutocorr < 0.1 ? '(偏低)' : '(正常)'}`
      : '-';
    const issueHtml = (r.issues && r.issues.length)
      ? r.issues.map(t => {
          const cls = t.includes('致命') ? 'err' : (t.includes('警告') ? 'warn' : '');
          return `<div class="msg ${cls}" style="margin-top:4px">${esc(t)}</div>`;
        }).join('')
      : '<div class="msg" style="margin-top:6px">自检全部通过</div>';
    const policyHtml = r.policy
      ? `<div class="kv"><span>规则最小里程</span><b>${r.policy.minDistance} m</b></div>
         <div class="kv"><span>规则有效时长</span><b>${r.policy.validTime} s</b></div>`
      : '';
    const fenceHtml = r.fence
      ? `<div class="kv"><span>围栏</span><b class="${r.fence.fallback ? 'warn' : 'ok'}">${esc(r.fence.name)}${r.fence.fallback ? `（兜底 ${Math.round(r.fence.distance)}m）` : ''}</b></div>`
      : '<div class="kv"><span>围栏</span><b class="warn">未取到</b></div>';
    const cpHtml = r.checkpointTotal != null
      ? `<div class="kv"><span>打卡点</span><b class="${r.checkpointPassed === r.checkpointTotal ? 'ok' : 'err'}">${r.checkpointPassed || 0} / ${r.checkpointTotal} 经过</b></div>`
      : '';
    $('planOut').innerHTML = `
      <div class="kv"><span>模式</span><b>${r.online ? '已连服务端' : '本地合成'} · ${modeTxt}</b></div>
      ${fenceHtml}
      ${cpHtml}
      ${policyHtml}
      <div class="kv"><span>实际里程</span><b>${r.actualLength} m</b></div>
      <div class="kv"><span>轨迹点数</span><b>${r.pointCount}</b></div>
      <div class="kv"><span>地图</span><b class="ok">${r.coords && r.coords.length ? `已画 ${r.coords.length} 点（取消勾选「预览轨迹」可隐藏）` : '没有轨迹点可画'}</b></div>
      <div class="kv"><span>时间桶</span><b>${r.bucketCount}</b></div>
      <div class="kv"><span>平均步频</span><b>${(r.avgCadence ?? 0).toFixed(1)} 步/分</b></div>
      <div class="kv"><span>步数自相关</span><b>${acTxt}</b></div>
      ${(() => {
        // 实际走的支路。请求里选「操场绕圈」不代表真绕成操场——打卡点不像
        // 操场时算法会退回路线规划，不显示出来用户会以为自己跑的是标准跑道。
        if (!r.trackMode) return '';
        const m = { stadium: '操场绕圈', route: '围栏路线', draw: '手绘轨迹' }[r.trackMode] || r.trackMode;
        const warn = r.stadiumFallback;
        return `<div class="kv"><span>实际路线</span><b class="${warn ? 'err' : 'ok'}">${esc(m)}${warn ? '（操场判定失败，已退回路线规划）' : ''}</b></div>`;
      })()}
      <div class="kv"><span>自检</span><b class="${r.blocked ? 'err' : 'ok'}">${esc(r.issueSummary)}</b></div>
      ${issueHtml}
      ${r.logs && r.logs.length ? `<details style="margin-top:8px"><summary style="cursor:pointer;color:var(--dim)">流程日志（${r.logs.length} 条）</summary><pre class="mono" style="font-size:11px;max-height:200px;overflow:auto;white-space:pre-wrap">${esc(r.logs.join('\n'))}</pre></details>` : ''}
      <div class="note">${esc(r.note)}</div>`;
    drawPreview(r.coords, r.fence, r.checkpoints);
  } catch (e) {
    setOut(e.message, true);
  } finally {
    busy(btn, false);
  }
};
/* ---------- 记分跑：学期进度 ---------- */
$('semBtn').onclick = async () => {
  const btn = $('semBtn');
  btn.disabled = true;
  $('semOut').innerHTML = '<div class="hint"><span class="spin"></span> 查询中…</div>';
  try {
    const r = await api('/api/semester');
    if (r.summaryError) {
      $('semOut').innerHTML = `<div class="msg err">${esc(r.summaryError)}</div>`;
      return;
    }
    const sm = r.summary || {};
    const g = r.goal || {};
    const done = sm.semesterValidDis || 0;
    const target = g.targetDis || 0;
    const pct = target > 0 ? Math.min(100, done / target * 100) : 0;
    $('semOut').innerHTML = `
      <div class="kv"><span>学期</span><b>${esc(sm.sname || '—')}</b></div>
      <div class="kv"><span>有效里程</span><b>${(done/1000).toFixed(2)} km</b></div>
      <div class="kv"><span>累计里程</span><b>${((sm.semesterDis||0)/1000).toFixed(2)} km</b></div>
      ${(g.goalMethod === 2 && !(sm.semesterCount || 0)) ? `
      <div class="kv"><span>次数</span><b class="warn">本校按里程考核，服务端不统计次数</b></div>` : `
      <div class="kv"><span>有效次数</span><b>${sm.semesterValidCount || 0} 次</b></div>
      <div class="kv"><span>累计次数</span><b>${sm.semesterCount || 0} 次</b></div>`}
      <div class="kv"><span>有效率</span><b class="${(sm.validRatio||0) < 0.9 ? 'warn' : 'ok'}">${((sm.validRatio||0)*100).toFixed(1)}%</b></div>
      ${target > 0 ? `
        <div class="kv"><span>目标</span><b>${(target/1000).toFixed(1)} km / ${g.targetCount || 0} 次</b></div>
        <div class="kv"><span>还差</span><b class="${pct >= 100 ? 'ok' : 'warn'}">${Math.max(0, (target-done)/1000).toFixed(2)} km</b></div>
        <div class="kv"><span>学期剩余</span><b class="${g.daysLeft < 7 ? 'err' : ''}">${g.daysLeft >= 0 ? g.daysLeft + ' 天' : '已结束'}</b></div>
        <div style="margin-top:10px;height:8px;background:var(--line-soft);border-radius:4px;overflow:hidden">
          <div style="width:${pct}%;height:100%;background:${pct>=100?'var(--ok)':'var(--accent)'}"></div>
        </div>
        <div class="hint" style="margin-top:6px">完成 ${pct.toFixed(1)}%</div>
      ` : '<div class="hint" style="margin-top:8px">未取到学期目标</div>'}
      ${r.goalError ? `<div class="msg warn" style="margin-top:6px">${esc(r.goalError)}</div>` : ''}`;
  } catch (e) {
    $('semOut').innerHTML = `<div class="msg err">${esc(e.message)}</div>`;
  } finally {
    btn.disabled = false;
  }
};

