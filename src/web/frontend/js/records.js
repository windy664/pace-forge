/* 记录列表、OBS 轨迹、单条详情与坐标探测。加载顺序约束见 js/core.js 头注释。 */
/* ---------- 记录列表 ---------- */
async function loadRecords() {
  $('list').innerHTML = '<div class="empty"><span class="spin"></span> 加载中…</div>';
  try {
    const data = await api('/api/records?page=1&size=20&type=1');
    const rows = pickRows(data);
    if (!rows.length) {
      $('list').innerHTML = '<div class="empty">这个学期还没有跑步记录</div>';
      return;
    }
    $('list').innerHTML = rows.map((r, i) => recHtml(r, i)).join('');
    [...document.querySelectorAll('.rec')].forEach((el, i) => {
      el.onclick = () => selectRec(rows[i], el);
      // 键盘用户也要能开记录：原来是纯 div + onclick，Tab 根本走不到
      el.onkeydown = (ev) => {
        if (ev.key === 'Enter' || ev.key === ' ') {
          ev.preventDefault();
          selectRec(rows[i], el);
        }
      };
    });
    // rrid 点一下复制。它挂在 .rec 里面，不拦一下的话点它也会选中整条记录。
    [...document.querySelectorAll('.rec .rid')].forEach(el => {
      el.onclick = async (ev) => {
        ev.stopPropagation();
        const id = el.dataset.rrid;
        try {
          await navigator.clipboard.writeText(id);
          const t = el.textContent;
          el.textContent = '已复制 ' + id;
          setTimeout(() => { el.textContent = t; }, 1200);
        } catch {
          // 非 https 或没权限时 clipboard 不可用，退回选中文本让用户手动复制
          const r = document.createRange();
          r.selectNodeContents(el);
          const sel = getSelection();
          sel.removeAllRanges();
          sel.addRange(r);
        }
      };
    });
    selectRec(rows[0], document.querySelector('.rec'));
  } catch (e) {
    $('list').innerHTML = '<div class="empty" style="color:var(--bad)">' + esc(e.message) + '</div>';
  }
}

/* 里程读数：统一成公里。
 * 服务端给米（totalDis=2000），个别接口给公里（2.5），单看数字分不清，
 * 用 200 做阈值：跑步记录不可能小于 0.2 km，小于它的一定是公里口径。
 * 后端已经补了 distanceKm，这里是第二道保险（后端没更新时前端也能显示对）。 */
function km(obj, ...keys) {
  if (obj && obj.distanceKm != null && keys[0] === 'totalDistance') return num(obj.distanceKm);
  for (const k of keys) {
    const v = num(obj?.[k]);
    if (v > 0) return v < 200 ? v : v / 1000;
  }
  return 0;
}

/* 列表响应结构在不同版本里可能是 {records:[...]} / {list:[...]} / 直接数组 */
function pickRows(d) {
  if (Array.isArray(d)) return d;
  for (const k of ['records', 'list', 'rows', 'data', 'items']) {
    if (Array.isArray(d?.[k])) return d[k];
  }
  return [];
}

function recHtml(r, i) {
  // 服务端字段是 totalDis（米），后端已归一化出 totalDistance/distanceKm；
  // 这里再兜一层并做量级判断，免得哪天出口改名又变回 0.00 km。
  const dist = km(r, 'totalDistance', 'totalDis', 'total_distance', 'distance');
  const secs = int(r.totalTime ?? r.total_time ?? r.runTime);
  // 服务端判定字段是 complete（bool）。以前读 isValid / status===1，
  // 而服务端根本不给这两个 —— undefined 恒为假，于是每条都显示「未达标」，
  // 哪怕 complete=true、statusInfo 写着「太棒了，恭喜您完成了跑步」。
  const valid = r.complete ?? r.completed ?? r.isValid ?? r.is_valid ?? false;
  const start = r.startTime ?? r.start_time;
  // rrid 必须显示出来。之前它只在代码里当参数用，界面上从来没有过——
  // 结果让用户「把那条记录的 rrid 发我」时，他根本没有任何地方能看到它。
  // 报 issue 时这是唯一能把一条记录指认出来的东西。
  const rid = r.rrid ?? r.id ?? r.recordId;
  return `<div class="rec" data-i="${i}" role="button" tabindex="0"
    aria-label="${dist.toFixed(2)} 公里，${fmtDur(secs)}，${valid ? '有效' : '未达标'}">
    <div class="r1">
      <span class="dist">${dist.toFixed(2)} km</span>
      <span class="tag ${valid ? 'ok' : 'bad'}">${valid ? '有效' : '未达标'}</span>
    </div>
    <div class="r2">
      <span>${fmtDate(start)}</span>
      <span>${fmtDur(secs)}</span>
      <span>${pace(dist, secs)}</span>
    </div>
    ${rid ? `<div class="rid" data-rrid="${esc(rid)}" title="点击复制 rrid">rrid ${esc(rid)}</div>` : ''}
  </div>`;
}

/* ---------- 从 OBS 拉轨迹 ----------
 * 服务端详情接口对工具提交的记录不返回坐标（jsonFromObs=1 → App 查本地库），
 * 但轨迹对象确实在对象存储里，key 规则是 run_data/{rrid/1000000}/{rrid}.json。
 * 用后端 /api/obs/fetch 换 Get 签名下载，浏览器侧用 DecompressionStream('gzip')
 * 解压，再取 run_data.allLocJson。
 */
async function ungzipB64(b64) {
  const bin = Uint8Array.from(atob(b64), c => c.charCodeAt(0));
  const out = new Blob([bin]).stream().pipeThrough(new DecompressionStream('gzip'));
  return await new Response(out).text();
}

/// 把毫秒戳按给定时区偏移格成 YYYYMMDDHH，用来拼 objectKey。
function stampHour(ms, offSec) {
  const d = new Date(ms + offSec * 1000);
  const p = n => String(n).padStart(2, '0');
  return d.getUTCFullYear() + p(d.getUTCMonth() + 1) + p(d.getUTCDate()) + p(d.getUTCHours());
}

/// 从对象存储拉轨迹。**候选 key 逐个试**。
///
/// 这里原来只试 `run_data/{rrid/1000000}/{rrid}.json` 一种，但上传侧用的
/// 是 `run_data/{YYYYMMDDHH}/{uuid}.json`——两套根本不是同一个东西。
/// 一个用 rrid 当文件名、拿前 4 位当「小时段」；另一个用时间戳当小时段、
/// uuid 当文件名。只试一种，另一边传上去就等于白传。
///
/// 所以按可能性从高到低排一串候选，命中就停。真机记录的 key 规则始终
/// 没有硬证据（obs.rs 和前端注释各说一套，都说自己是实测），穷举比我
/// 们继续猜便宜——每个对象才几 KB。
async function obsTrack(rrid, detail) {
  const d = detail || {};
  const uuid = d.uuid || d.runUuid || d.run_uuid || d.fileId;
  const st = num(d.startTime) || num(d.start_time);

  const cands = [];
  // ① 真机格式（前端注释的说法）：rrid 前 4 位当小时段
  cands.push('run_data/' + Math.floor(rrid / 1000000) + '/' + rrid + '.json');
  // ② 我们上传的格式：本地时区（UTC+8）小时段 + uuid
  if (uuid && st) cands.push('run_data/' + stampHour(st, 8 * 3600) + '/' + uuid + '.json');
  // ③ 同一套的 UTC 版本（上传时两份都传了）
  if (uuid && st) cands.push('run_data/' + stampHour(st, 0) + '/' + uuid + '.json');
  // ④ rrid 前 6 位（万一小时段是 YYYYMM 而不是 YYYYMMDD）
  cands.push('run_data/' + Math.floor(rrid / 10000) + '/' + rrid + '.json');
  // ⑤ 兜底目录
  if (uuid && st) cands.push('runData/' + (d.uid || '') + '/' + (d.sportType || 1) + '/' + Math.floor(st / 1000) + '_' + uuid + '.txt');

  for (const key of cands) {
    try {
      const j = await api('/api/obs/fetch', { method: 'POST', body: JSON.stringify({ key }) });
      const obj = j.data ?? j;
      if (!obj || !obj.run_data) continue;
      const wrap = JSON.parse(await ungzipB64(obj.run_data));
      const raw = wrap.allLocJson;
      const arr = typeof raw === 'string' ? JSON.parse(raw) : raw;
      if (!Array.isArray(arr) || arr.length < 2) continue;
      // 走和 findTrack 同一个解析器：gLat/gLng 优先、占位（-1/0）剔除、
      // 输出 [lat, lng]。
      // 原来这里是 `num(p.gLat) || num(p.lat)` + `.filter(p => p.lat && p.lng)`：
      // 判的是「有没有值」，而 -1 是真值，于是 -1,-1 全留下来，整条轨迹
      // 又被画回几内亚湾——兜底路径本身也是坏的。
      const pts = parseCoordArray(arr);
      if (pts.length > 1) {
        console.log('[obs] 命中 key:', key, '点数', pts.length);
        return pts;
      }
    } catch (e) {
      // 404 就是没这个对象，继续试下一个
    }
  }
  console.warn('[obs] 全部候选 key 都没取到轨迹:', cands);
  return null;
}

/* ---------- 单条详情 ---------- */
async function selectRec(r, el) {
  document.querySelectorAll('.rec').forEach(x => x.classList.remove('sel'));
  if (el) el.classList.add('sel');
  const rrid = r.rrid ?? r.id ?? r.recordId;
  layer.clearLayers();
  $('detail').innerHTML = '<span class="spin"></span> 加载轨迹…';
  if (!rrid) { $('detail').textContent = '这条记录没有 rrid'; return; }

  try {
    const d = await api('/api/record/' + rrid);
    let pts = findTrack(d);
    // 服务端详情不带轨迹点（jsonFromObs=1 时 App 也去查手机本地库），
    // 但轨迹对象一直在 OBS 里：key 是 run_data/{rrid/1000000}/{rrid}.json。
    // 拉下来 → gzip 解压 → 取 run_data.allLocJson → 就能画线。
    //
    // 兜底判据是「**有没有有效点位**」，不是「数组里有没有东西」：
    // findTrack → parseCoordArray 只会吐出通过 valid() 的点，所以 pts.length
    // 就是有效点数，0 才代表接口真的没带坐标。以前写的是 pts.length <= 1，
    // 而 -1,-1 也算有效点（|-1| ≤ 90 当然过），114 个假点把数组塞满，
    // 这里永远判不出「没点位」，OBS 兜底就再也走不到了。
    if (pts.length === 0) {
      $('detail').innerHTML = '<span class="spin"></span> 从 OBS 拉轨迹…';
      const obs = await obsTrack(rrid, d);
      if (obs && obs.length) pts = obs;
    }
    if (pts.length > 1) {
      const line = L.polyline(pts, { color: '#4493f8', weight: 4, opacity: .9 }).addTo(layer);
      L.circleMarker(pts[0], { radius: 6, color: '#3fb950', fillOpacity: 1 }).addTo(layer);
      L.circleMarker(pts[pts.length - 1], { radius: 6, color: '#f85149', fillOpacity: 1 }).addTo(layer);
      map.fitBounds(line.getBounds(), { padding: [24, 24] });
      map.invalidateSize();
      $('detail').innerHTML = detailHtml(d, pts.length);
    } else {
      // 画不出线时**不要移动地图**。这里原来 setView 到成都 (30.5,104.0)，
      // 一个东北某平原校区的记录点进去，地图直接跳到四川，比不画线更迷惑。
      // 保持当前视图即可，用户自己知道这条记录在哪跑的。
      $('detail').innerHTML = detailHtml(d, pts.length) +
        (pts.length === 1
          ? '<div style="margin-top:8px;color:var(--dim)">这条记录只有 1 个有效点位，连不成线。' +
            '（按「有有效点位就不去 OBS」的规则，这里不再兜底。）</div>'
          : '<div style="margin-top:8px;color:var(--dim)">OBS 里也没有这条记录的轨迹对象：' +
            '要么当时上传失败，要么 objectKey 对不上（旧版时区不稳，实测 11-07 那批 ' +
            'UTC+8 的 key 全是 404）。这条记录在哪儿都画不出线。</div>');
    }
  } catch (e) {
    $('detail').innerHTML = '<span style="color:var(--bad)">' + esc(e.message) + '</span>';
  }
}

function detailHtml(d, n) {
  const g = (k, alt) => d[k] ?? alt;
  const dist = km(d, 'totalDistance', 'totalDis', 'total_distance');
  const vdist = km(d, 'validDistance', 'validDis', 'valid_distance');
  const secs = int(g('totalTime', g('total_time')));
  const vsecs = int(g('validTime', g('valid_time')));
  const cal = int(g('calorie'));
  const freq = int(g('avgStepFreq', g('avg_step_freq', g('stepFreq'))));
  const reason = g('unCompleteReason', g('uncomplete_reason'));
  // reasonList 是逐条规则的判定：complete=false 的那几条就是没达标的原因
  const reasons = Array.isArray(d.reasonList)
    ? d.reasonList.filter(x => x && x.complete === false && x.reason).map(x => x.reason)
    : [];
  // rrid 放第一行：报 issue 时它是最常被要的东西，藏在列表里翻起来麻烦
  const rid = g('rrid', g('recordId', g('id')));
  const rows = [
    ...(rid ? [['记录编号', 'rrid ' + rid]] : []),
    ['总距离', dist.toFixed(2) + ' km'],
    ['有效距离', vdist.toFixed(2) + ' km'],
    ['总时长', fmtDur(secs)],
    ['有效时长', fmtDur(vsecs)],
    ['配速', pace(dist, secs)],
    ['步频', freq ? freq + ' 步/分' : '—'],
    ['消耗', cal ? cal + ' 千卡' : '—'],
    ['轨迹点', n ? n + ' 个' : '无'],
  ];
  if (reasons.length) rows.push(['未通过规则', reasons.join('、')]);
  else if (reason) rows.push(['未达标原因', String(reason)]);
  // jsonFromObs=1 表示这条记录的轨迹在 App 本地库里，服务端不存点——详情页
  // 拉不到坐标是正常的，不是上传失败（见 docs/notes/TRAJECTORY-DISPLAY.md）。
  if (d.jsonFromObs === 1 && n === 0) {
    rows.push(['轨迹来源', 'App 本地库（服务端不存轨迹点）']);
  }
  return '<div class="kv">' + rows.map(([k, v]) => `<span>${k}</span><span>${esc(String(v))}</span>`).join('') + '</div>';
}

/* ---------- 轨迹自动探测 ----------
 * 服务端字段名不稳定，递归扫描 JSON：
 *   1. 形如 "lng,lat,lng,lat,..." 的字符串
 *   2. 形如 [[lng,lat], ...] 的数组
 *   3. 形如 [{lat,lng|lon|latitude,longitude}, ...] 的对象数组
 *      —— 真机那批点位是 {lat:-1, lng:-1, gLat, gLng}，字段名与占位值都由
 *      coordOf 处理（gLat/gLng 优先，-1/0 一律当占位丢弃）。
 * 取**有效点数最多**的那一个（parseCoordArray 只返回通过 valid() 的点，
 * 所以「数组很长但全是 -1」不会再赢过一段短的真轨迹）。
 */
function findTrack(obj) {
  let best = [];
  const visit = (v, depth) => {
    if (depth > 6 || v == null) return;
    if (typeof v === 'string') {
      const pts = parseCoordString(v);
      if (pts.length > best.length) best = pts;
      return;
    }
    if (Array.isArray(v)) {
      const pts = parseCoordArray(v);
      if (pts.length > best.length) best = pts;
      v.forEach(x => visit(x, depth + 1));
      return;
    }
    if (typeof v === 'object') {
      Object.values(v).forEach(x => visit(x, depth + 1));
    }
  };
  visit(obj, 0);
  return best;
}

/// 占位坐标的哨兵值。
///
/// 真机的点位对象长这样（OBS 里 25 个字段那份，见
/// re/zjwh740/evidence/obs_real_20261002.json）：
///   { lat: -1.0, lng: -1.0, gLat: "48.123456", gLng: "104.123456", ... }
/// `lat`/`lng` 固定填 -1，是 App 的「未定位」哨兵；真实坐标在 gLat/gLng。
/// 服务端另有一批响应把 lat/lon 填 0（api/coord.rs 顶部注释写了）。
/// 这两个值都不是坐标，不能拿来画图、也不能算「有点位」。
function isPlaceholderCoord(v) {
  return v === 0 || v === -1;
}

function valid(lat, lng) {
  return Number.isFinite(lat) && Number.isFinite(lng) &&
         Math.abs(lat) <= 90 && Math.abs(lng) <= 180 &&
         !isPlaceholderCoord(lat) && !isPlaceholderCoord(lng);
}

/// 取一个坐标字段的数值。空值给 NaN（不是 0，0 在这里是哨兵），字符串
/// "48.123456" 也认——真机把 gLat/gLng 序列化成字符串，这里必须能转。
function coordNum(v) {
  if (v === null || v === undefined || v === '') return NaN;
  return Number(v);
}

/// 从任意点位对象里取出 [lat, lng]（有效坐标）；取不到给 null。
///
/// 规则：**成对**按优先级找，第一对通过 valid() 的就是它——
///   ① gLat/gLng   真机与 OBS 的真实坐标（GCJ-02，可能是字符串）
///   ② gcjLat/gcjLng  /api/plan 的打卡点（GCJ-02，对得上高德瓦片）
///   ③ glat/glng、glat/glon   围栏 / 采样点的 GCJ-02 写法
///   ④ lat/lng、lat/lon、latitude/longitude   兜底（多为 BD-09，且常是占位）
/// 必须成对取：gLat 是 GCJ-02、lat 是 BD-09，跨套拼出来的点会差几百米。
/// 一对都过不了 valid()（比如全是 -1）就返回 null——这个点没有坐标。
/// 点是否在多边形内（射线法）。和 Rust 那边 `fence_route::contains`
/// 同一套判定，用于在**保存禁区时**就发现「这块盖住了打卡点」。
function pointInPoly(poly, lat, lng) {
  if (!Array.isArray(poly) || poly.length < 3) return false;
  let inside = false;
  for (let i = 0, j = poly.length - 1; i < poly.length; j = i++) {
    const yi = poly[i][0], xi = poly[i][1];
    const yj = poly[j][0], xj = poly[j][1];
    if (((yi > lat) !== (yj > lat)) && (lng < (xj - xi) * (lat - yi) / (yj - yi) + xi)) {
      inside = !inside;
    }
  }
  return inside;
}

function coordOf(p) {
  if (!p || typeof p !== 'object') return null;
  const pairs = [
    [p.gLat, p.gLng],
    [p.gcjLat, p.gcjLng],
    [p.glat, p.glng], [p.glat, p.glon],
    [p.lat, p.lng], [p.lat, p.lon],
    [p.latitude, p.longitude], [p.latitude, p.lon],
  ];
  for (const [a, b] of pairs) {
    const lat = coordNum(a), lng = coordNum(b);
    if (valid(lat, lng)) return [lat, lng];
  }
  return null;
}

function parseCoordString(s) {
  if (s.length < 20 || !/[\d.]/.test(s)) return [];
  // 常见：lng,lat,lng,lat,... 或 lng,lat;lng,lat
  const nums = s.split(/[;,\s]+/).map(Number).filter(n => Number.isFinite(n));
  if (nums.length < 4) return [];
  const out = [];
  for (let i = 0; i + 1 < nums.length; i += 2) {
    const a = nums[i], b = nums[i + 1];
    // 判断哪个是纬度：国内 lat 在 18~54，lng 在 73~136
    if (valid(b, a) && Math.abs(b) < 60) out.push([b, a]);
    else if (valid(a, b) && Math.abs(a) < 60) out.push([a, b]);
  }
  return out;
}

function parseCoordArray(arr) {
  if (!arr.length) return [];
  const first = arr[0];
  if (Array.isArray(first) && first.length >= 2) {
    const out = [];
    for (const p of arr) {
      const a = Number(p[0]), b = Number(p[1]);
      if (valid(b, a) && Math.abs(b) < 60) out.push([b, a]);
      else if (valid(a, b) && Math.abs(a) < 60) out.push([a, b]);
    }
    return out;
  }
  if (first && typeof first === 'object') {
    const out = [];
    for (const p of arr) {
      // 字段名交给 coordOf：gLat/gLng 优先，占位（-1/0）剔除。
      // 以前这里只认 lat/latitude + lng/lon/longitude，真机那批点位
      // lat=-1,lng=-1 全部通过 valid()，114 个假点照样当成有效轨迹。
      const ll = coordOf(p);
      if (ll) out.push(ll);
    }
    return out;
  }
  return [];
}

/* ---------- 格式化 ---------- */
const num = v => { const n = Number(v); return Number.isFinite(n) ? n : 0; };
const int = v => Math.round(num(v));
const esc = s => String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
function fmtDate(ms) {
  const n = num(ms);
  if (!n) return '—';
  const d = new Date(n < 1e12 ? n * 1000 : n);
  const p = x => String(x).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}
function fmtDur(sec) {
  const s = int(sec);
  if (!s) return '—';
  const m = Math.floor(s / 60);
  return `${m}′${String(s % 60).padStart(2, '0')}″`;
}
function pace(km, sec) {
  if (!km || !sec) return '—';
  const p = sec / km;
  const m = Math.floor(p / 60), s = Math.round(p % 60);
  return `${m}′${String(s).padStart(2, '0')}″/km`;
}

