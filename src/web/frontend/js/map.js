/* 地图初始化、校区定位、选点与坐标系换算。加载顺序约束见 js/core.js 头注释。 */
/* ---------- 地图 ---------- */
let map, layer, marker, previewLayer;
function initMap() {
  if (map) return;
  // 和 App 用同一套底图：App 里就是高德（DEX 里 com/amap/api/ 2564 处、
  // autonavi 600 处，com/baidu/mapapi/ 是空的）。
  //
  // 关键：高德瓦片是按 GCJ-02 网格切的，Leaflet 会把传进去的经纬度直接
  // 当作瓦片坐标算 z/x/y。所以喂给 Leaflet 的必须是 GCJ-02——点位用
  // gcjLat/gcjLng，手绘出来的也是 GCJ-02，不能混用 BD-09。
  // 初始视野不能钉死任何一所学校。这里原来写的是开发时那所校区的坐标，
  // 别人一打开就看到自己没去过的城市。先给全国范围，登录后由
  // fitToAccountArea() 按真实围栏自动缩到本校。
  map = L.map('map', { zoomControl: true }).setView([34.5, 108.9], 4);

  const roadMap = L.tileLayer(
    'https://webrd0{s}.is.autonavi.com/appmaptile?lang=zh_cn&size=1&scale=1&style=8&x={x}&y={y}&z={z}',
    { subdomains: '1234', maxZoom: 18, attribution: '© 高德地图' }
  );
  const satellite = L.tileLayer(
    'https://webst0{s}.is.autonavi.com/appmaptile?style=6&x={x}&y={y}&z={z}',
    { subdomains: '1234', maxZoom: 18, attribution: '© 高德地图' }
  );
  // 卫星图叠加路网标注，看着更像 App
  const satelliteLabel = L.tileLayer(
    'https://webst0{s}.is.autonavi.com/appmaptile?style=8&x={x}&y={y}&z={z}',
    { subdomains: '1234', maxZoom: 18 }
  );
  const satLayer = L.layerGroup([satellite, satelliteLabel]);

  roadMap.addTo(map);
  L.control.layers({ '高德路网': roadMap, '高德卫星': satLayer }, null,
    { position: 'topright' }).addTo(map);
  layer = L.layerGroup();
  if (!$('showRec') || $('showRec').checked) layer.addTo(map);
  initDrawing();
  initBlockers();
  // 定位控件单独初始化：原先挂在 initDrawing 末尾，而 initDrawing 开头有
  // `map.__drawBound` 守卫，一旦地图被重复初始化就再也不执行，「定位」按钮
  // 的点击处理器根本没绑上，点了毫无反应。
  initLoc();
  fitToAccountArea();
}

// 把视野挪到当前账号真实所在的校区。
//
// 没有围栏的学校（issue #2 那种 geoFences=null）拿不到边界，这时保持全国
// 视图不动——宁可不缩放，也不要猜一个坐标。
/// 围栏中心（BD-09 的多边形 → 简单平均）。
function fenceCenter(f) {
  const ring = Array.isArray(f && f.ring) ? f.ring : [];
  if (!ring.length) return null;
  let la = 0, ln = 0;
  for (const p of ring) {
    const a = Array.isArray(p) ? p[0] : p && p[0];
    const b = Array.isArray(p) ? p[1] : p && p[1];
    if (!Number.isFinite(a) || !Number.isFinite(b)) continue;
    la += a; ln += b;
  }
  return [la / ring.length, ln / ring.length];
}

/// 把账号下发的围栏填进「校区/场地」下拉。
///
/// 一个学校有多个校区时服务端会下发多个围栏，但界面原来只把它们并成一个
/// 视野框（fitBounds），没有任何地方能选——用户看到的可用场地就只有锚点
/// 碰巧落在的那一个。这里列出来，选中就把锚点挪过去，让
/// `fence::select_by_anchor` 真的换到那个校区。
function fillFenceSelect(fences) {
  const sel = $('fenceSel');
  if (!sel) return;
  const cur = sel.value;
  sel.innerHTML = '<option value="">校区/场地（先查一次）</option>';
  fences.forEach((f, i) => {
    const o = document.createElement('option');
    o.value = String(i);
    const nm = (f.name || '').trim();
    o.textContent = nm ? `${i + 1}. ${nm}` : `${i + 1}. ${Math.round((f.ring || []).length)} 点多边形`;
    sel.appendChild(o);
  });
  // 只有一个场地时不打扰，直接选中
  if (fences.length === 1) sel.value = '0';
  else if (cur && sel.querySelector(`option[value="${cur}"]`)) sel.value = cur;
  sel.dataset.count = String(fences.length);
  window.__fences = fences;
  if (fences.length > 1) {
    $('llInfo').textContent = `账号下有 ${fences.length} 个校区/场地，可在左上角切换`;
  }
}

async function fitToAccountArea() {
  try {
    const r = await api('/api/fences');
    const fences = r.fences || [];
    fillFenceSelect(fences);
    if (!fences.length || !map) return;
    const g = L.featureGroup();
    let added = 0;
    for (const f of fences) {
      const ring = fenceRing(f);
      if (ring.length < 3) continue;
      g.addLayer(L.polygon(ring, { opacity: 0, fillOpacity: 0 }));
      added++;
    }
    if (!added) return;
    const b = g.getBounds();
    if (b.isValid()) map.fitBounds(b, { padding: [40, 40] });
  } catch (e) {
    // 没登录或接口异常都走这里，保持全国视图即可
  }
}


/* 选了校区/场地：把定位锚点挪到该围栏中心。
 *
 * 锚点决定 `fence::select_by_anchor` 选哪个围栏——所以这一步等效于
 * 「换到那个校区的田径场」。围栏是 BD-09，填给输入框要转成 GCJ-02
 * （底图是高德），否则又会偏几百米。 */
document.addEventListener('change', (e) => {
  if (!e.target || e.target.id !== 'fenceSel') return;
  const idx = e.target.value;
  if (idx === '') return;
  const f = (window.__fences || [])[Number(idx)];
  const c = f && fenceCenter(f);
  if (!c) return;
  const bd2gcj = (la, ln) => {
    // 和后端 bd09_to_gcj02 同一套参数（BD09 → GCJ02 的反算）
    const x = ln - 0.0065, y = la - 0.006;
    const z = Math.sqrt(x * x + y * y) - 0.00002 * Math.sin(y * Math.PI * 3000 / 180);
    const t = Math.atan2(y, x) - 0.000003 * Math.cos(x * Math.PI * 3000 / 180);
    return [z * Math.sin(t), z * Math.cos(t)];
  };
  const [gl, go] = bd2gcj(c[0], c[1]);
  setPickLocation(gl, go, false);
  $('llCoord').value = 'gcj02';
  $('llInfo').textContent = `已切到「${(f.name || '场地 ' + (Number(idx) + 1)).trim()}」`;
  if (map && Number.isFinite(gl) && Number.isFinite(go)) {
    map.setView([gl, go], Math.max(map.getZoom(), 16));
  }
});

/* ---------- 校区定位（围栏拿不到时手动指定） ----------
 * 服务端围栏接口对部分学校是空的，没有中心点就规划不了。这里让人自己
 * 说校门在哪儿：手输，或者直接在地图上点一下。
 *
 * 这一带的底图是高德瓦片，Leaflet 的 latlng 数值就是 GCJ-02，所以点选
 * 出来的坐标标成 GCJ-02，交给后端统一转 BD-09。
 */
let pickMode = false;
let locMarker = null;

function readLocInputs() {
  const lat = parseFloat($('llLat').value);
  const lng = parseFloat($('llLng').value);
  if (!Number.isFinite(lat) || !Number.isFinite(lng)) return null;
  if (lat < -90 || lat > 90 || lng < -180 || lng > 180) return null;
  // -1 / 0 是占位哨兵（和点位解析同一套规则），填进来等于没填
  if (isPlaceholderCoord(lat) || isPlaceholderCoord(lng)) return null;
  return { lat: lat, lng: lng, coord: $('llCoord').value };
}

// 终点：留空 = 保持原来的闭环（跑完回起点）；填了 = 收在这里。
// 服务端要 BD-09，前端输入是底图坐标（GCJ-02），转换在后端做（与锚点同一套）。
let endPickMode = false;
let endMarker = null;
function readEndInputs() {
  const lat = parseFloat($('endLat').value);
  const lng = parseFloat($('endLng').value);
  if (!Number.isFinite(lat) || !Number.isFinite(lng)) return null;
  if (lat < -90 || lat > 90 || lng < -180 || lng > 180) return null;
  if (isPlaceholderCoord(lat) || isPlaceholderCoord(lng)) return null;
  return { lat: lat, lng: lng };
}
function endBody() {
  const v = readEndInputs();
  return v ? { endLat: v.lat, endLng: v.lng } : {};
}
function drawEndMarker() {
  if (!map) return;
  if (endMarker) { map.removeLayer(endMarker); endMarker = null; }
  const v = readEndInputs();
  if (!v) { $('endInfo').textContent = '留空时跑完绕回起点'; return; }
  endMarker = L.circleMarker([v.lat, v.lng], {
    radius: 7, color: tok('--danger'), fillColor: tok('--danger'), fillOpacity: .85, weight: 2,
  }).addTo(map).bindTooltip('终点');
  $('endInfo').textContent = '终点已设：跑完收在这里';
}
function setEndPickMode(on) {
  endPickMode = !!on;
  $('endPick').textContent = endPickMode ? '点地图上的终点…' : '在地图上点终点';
  if (map) map.getContainer().style.cursor = (endPickMode || pickMode) ? 'crosshair' : '';
  if (endPickMode && drawMode) setDrawMode(false);
  if (endPickMode && pickMode) setPickMode(false);
  if (endPickMode && blockMode) setBlockMode(false);
}

// 只把有值的字段塞进请求体；留空时后端照旧走围栏
function locBody() {
  const v = readLocInputs();
  return v ? { lat: v.lat, lng: v.lng, coord: v.coord } : {};
}

function setPickMode(on) {
  pickMode = !!on;
  $('llPick').textContent = pickMode ? '点地图上的位置…' : '在地图上点选';
  if (map) map.getContainer().style.cursor = pickMode ? 'crosshair' : '';
  // 和手绘、禁区互斥：一次点击不能被两个模式抢
  if (pickMode && drawMode) setDrawMode(false);
  if (pickMode && blockMode) setBlockMode(false);
}

function setPickLocation(lat, lng, fromMap) {
  $('llLat').value = lat.toFixed(6);
  $('llLng').value = lng.toFixed(6);
  if (fromMap) $('llCoord').value = 'gcj02';
  if (map) {
    if (locMarker) map.removeLayer(locMarker);
    // 用十字 + 常显标签，**不要**再用圆圈：打卡点是圆圈（绿/红/灰），
    // 锚点也画圆圈的话，用户会把橙色的那个数成「第 5 个打卡点」。
    locMarker = L.marker([lat, lng], {
      icon: L.divIcon({
        className: 'anchor-icon',
        html: '<div class="anchor-cross"></div><div class="anchor-label">校区中心</div>',
        iconSize: [64, 40], iconAnchor: [32, 20],
      }),
      keyboard: false,
    }).addTo(map);
  }
  $('llInfo').textContent = fromMap ? '已取到地图点（GCJ-02），提交时自动换算' : '';
}

function clearLoc() {
  $('llLat').value = '';
  $('llLng').value = '';
  if (locMarker && map) { map.removeLayer(locMarker); locMarker = null; }
  $('llInfo').textContent = '已清空，改回用围栏中心';
}

// 把输入框里的坐标同步到地图：画标记 + 把视野移过去。
// 少了这一步，用户填完坐标看地图还是原来那片区域，会以为没生效。
/* ---------- 坐标系换算（与 api/coord.rs 同一套公式） ----------
 * 地图用的是高德瓦片，Leaflet 的 latlng 必须按 GCJ-02 网格解释。用户在
 * 下拉里选百度（BD-09）或 GPS 原始（WGS-84）时，直接把数字丢给 setView
 * 会跳到偏几百米的地方——看着像「没跳到我说的地方」。所以先换算再定位。
 */
const COORD_PI = Math.PI;
const COORD_A = 6378245.0;
const COORD_EE = 0.00669342162296594323;
const COORD_X_PI = Math.PI * 3000.0 / 180.0;

function _outOfChina(lat, lng) {
  return lng < 72.004 || lng > 137.8347 || lat < 0.8293 || lat > 55.8271;
}
function _tLat(x, y) {
  let r = -100.0 + 2.0 * x + 3.0 * y + 0.2 * y * y + 0.1 * x * y + 0.2 * Math.sqrt(Math.abs(x));
  r += (20.0 * Math.sin(6.0 * x * COORD_PI) + 20.0 * Math.sin(2.0 * x * COORD_PI)) * 2.0 / 3.0;
  r += (20.0 * Math.sin(y * COORD_PI) + 40.0 * Math.sin(y / 3.0 * COORD_PI)) * 2.0 / 3.0;
  r += (160.0 * Math.sin(y / 12.0 * COORD_PI) + 320 * Math.sin(y * COORD_PI / 30.0)) * 2.0 / 3.0;
  return r;
}
function _tLng(x, y) {
  let r = 300.0 + x + 2.0 * y + 0.1 * x * x + 0.1 * x * y + 0.1 * Math.sqrt(Math.abs(x));
  r += (20.0 * Math.sin(6.0 * x * COORD_PI) + 20.0 * Math.sin(2.0 * x * COORD_PI)) * 2.0 / 3.0;
  r += (20.0 * Math.sin(x * COORD_PI) + 40.0 * Math.sin(x / 3.0 * COORD_PI)) * 2.0 / 3.0;
  r += (150.0 * Math.sin(x / 12.0 * COORD_PI) + 300.0 * Math.sin(x / 30.0 * COORD_PI)) * 2.0 / 3.0;
  return r;
}
function wgs84ToGcj02(lat, lng) {
  if (_outOfChina(lat, lng)) return [lat, lng];
  let dLat = _tLat(lng - 105.0, lat - 35.0);
  let dLng = _tLng(lng - 105.0, lat - 35.0);
  const radLat = lat / 180.0 * COORD_PI;
  let magic = Math.sin(radLat);
  magic = 1 - COORD_EE * magic * magic;
  const sqrtMagic = Math.sqrt(magic);
  dLat = (dLat * 180.0) / ((COORD_A * (1 - COORD_EE)) / (magic * sqrtMagic) * COORD_PI);
  dLng = (dLng * 180.0) / (COORD_A / sqrtMagic * Math.cos(radLat) * COORD_PI);
  return [lat + dLat, lng + dLng];
}
function gcj02ToBd09(lat, lng) {
  const z = Math.sqrt(lng * lng + lat * lat) + 0.00002 * Math.sin(lat * COORD_X_PI);
  const t = Math.atan2(lat, lng) + 0.000003 * Math.cos(lng * COORD_X_PI);
  return [z * Math.sin(t) + 0.006, z * Math.cos(t) + 0.0065];
}
function bd09ToGcj02(lat, lng) {
  const x = lng - 0.0065, y = lat - 0.006;
  const z = Math.sqrt(x * x + y * y) - 0.00002 * Math.sin(y * COORD_X_PI);
  const t = Math.atan2(y, x) - 0.000003 * Math.cos(x * COORD_X_PI);
  return [z * Math.sin(t), z * Math.cos(t)];
}

// 任意坐标系 → GCJ-02（地图能直接定位的那套）
function toGcj02(lat, lng, sys) {
  if (sys === 'bd09') return bd09ToGcj02(lat, lng);
  if (sys === 'wgs84') return wgs84ToGcj02(lat, lng);
  return [lat, lng];
}

function syncLocToMap() {
  if (!map) return;
  const v = readLocInputs();
  if (!v) {
    if (locMarker) { map.removeLayer(locMarker); locMarker = null; }
    $('llInfo').textContent = '';
    return;
  }
  // 换算到 GCJ-02 再定位：地图是高德瓦片，数值必须按 GCJ-02 解释
  try {
    const g = toGcj02(v.lat, v.lng, v.coord);
    if (locMarker) map.removeLayer(locMarker);
    locMarker = L.marker(g, {
      icon: L.divIcon({
        className: 'anchor-icon',
        html: '<div class="anchor-cross"></div><div class="anchor-label">校区中心</div>',
        iconSize: [64, 40], iconAnchor: [32, 20],
      }),
      keyboard: false,
    }).addTo(map);
    // 容器显示出来之前尺寸是 0，setView 会算错中心，先让它重新量一次
    map.invalidateSize();
    // 已经放大得比 15 级更近就别缩回去，避免每次都把手动缩放重置
    const z = Math.max(map.getZoom() || 15, 15);
    map.setView(g, z);
    $('llInfo').textContent = v.coord === 'gcj02'
      ? '已定位到 ' + g[0].toFixed(5) + ', ' + g[1].toFixed(5)
      : '已定位到 ' + g[0].toFixed(5) + ', ' + g[1].toFixed(5) +
        '（' + v.coord.toUpperCase() + ' 已换算到高德底图）';
  } catch (e) {
    $('llInfo').textContent = '定位失败：' + e.message;
  }
}

let locSyncTimer = null;

/// 查围栏 + 必到点（不生成轨迹）。
///
/// 锚点用「纬度/经度」里填的值（按「坐标来自」换算成 GCJ-02 传给后端），
/// 留空就让后端取围栏质心。
async function fetchFencesAndPoints() {
  const info = $('llInfo');
  const btn = $('llFetch');
  if (btn) { btn.disabled = true; btn.textContent = '查询中…'; }
  if (info) info.textContent = '正在拉围栏与点位…';
  try {
    const a = readLocInputs();
    const qs = new URLSearchParams();
    if (a) {
      const g = toGcj02(a.lat, a.lng, a.coord);
      qs.set('lat', g[0].toFixed(6));
      qs.set('lng', g[1].toFixed(6));
    }
    const d = $('rDist') ? $('rDist').value : '';
    if (d) qs.set('distance', String(d));
    const r = await api('/api/points?' + qs.toString());
    const points = (r.points || []).map(p => ({
      index: p.index,
      gcjLat: p.gcjLat,
      gcjLng: p.gcjLng,
      name: p.name,
      // 没跑轨迹，过没过不知道就别说「未经过」
      passed: null,
      nearest: null,
    }));
    drawPreview(null, r.fence, points);
    const bits = [];
    bits.push(r.fence && fenceRing(r.fence).length >= 3
      ? `围栏「${r.fence.name || r.fence.id}」`
      : '未取到围栏');
    // 说清楚「那一圈颜色是什么」，否则用户会把橙色的校区中心数成第 5 个点
    bits.push(`${points.length} 个必到点（灰虚线＝还没跑，不知道过没过）`);
    bits.push('橙十字＝校区中心');
    if (r.anchorFrom) bits.push('锚点：' + r.anchorFrom);
    if (info) info.textContent = bits.join(' · ');
    if (!points.length) {
      setOut('查到了围栏但服务端没下发必到点。这种校区（issue #2 那类）没有点位，只能手绘或让算法按围栏自由规划。');
    }
  } catch (e) {
    if (info) info.textContent = '查询失败：' + e.message;
  } finally {
    if (btn) { btn.disabled = false; btn.textContent = '查围栏/点位'; }
  }
}

function initLoc() {
  if (!map || map.__locBound) return;
  map.__locBound = true;

  // 明确的「定位」：填完数字点一下，地图立刻跳过去。
  // 防抖那套是打字时顺手跟着动，但用户要的是一个确定的动作，
  // 点下去必须立刻有反应，不能等 500 毫秒。
  $('llGo').onclick = () => {
    const v = readLocInputs();
    if (!v) {
      $('llInfo').textContent = '纬度和经度都要填，范围别越界';
      return;
    }
    syncLocToMap();
  };
  $('llPick').onclick = () => setPickMode(!pickMode);
  $('endPick').onclick = () => setEndPickMode(!endPickMode);
  $('endClear').onclick = () => {
    $('endLat').value = '';
    $('endLng').value = '';
    drawEndMarker();
  };
  ['endLat', 'endLng'].forEach(id => { $(id).onchange = drawEndMarker; });
  $('llClear').onclick = clearLoc;

  // 打字过程中不要每敲一位就跳一次地图，停下来再同步
  ['llLat', 'llLng'].forEach(id => {
    $(id).oninput = () => {
      $('llInfo').textContent = '';
      clearTimeout(locSyncTimer);
      locSyncTimer = setTimeout(syncLocToMap, 500);
    };
    // 回车等同点「定位」，输完直接敲回车最顺手
    $(id).onkeydown = ev => {
      if (ev.key === 'Enter') {
        ev.preventDefault();
        clearTimeout(locSyncTimer);
        $('llGo').click();
      }
    };
  });
  $('llCoord').onchange = () => syncLocToMap();

  // 点选改走 Leaflet 的 click：它自己区分了「拖动地图」和「点一下」，
  // 比在 pointerdown 上抢事件可靠——拖动结束时的 pointerdown 会被误判成点选。
  map.on('click', ev => {
    if (endPickMode) {
      const ll = ev.latlng;
      $('endLat').value = ll.lat.toFixed(6);
      $('endLng').value = ll.lng.toFixed(6);
      setEndPickMode(false);
      drawEndMarker();
      return;
    }
    if (!pickMode) return;
    setPickLocation(ev.latlng.lat, ev.latlng.lng, true);
    setPickMode(false);
  });

  // 绑定成功留个可见痕迹：看不到这句说明控件没初始化，别去猜别的原因
  const info = $('llInfo');
  // 只查围栏 + 必到点，不生成轨迹。
  //
  // 这按钮存在的理由：围栏和点位以前只出现在 /api/plan 的响应里，所以想
  // 看见自己学校的电子围栏，必须先画一条线、跑完一次规划——issue #3 里
  // 「得先在手绘图上生成轨迹之后才能看到点位」就是这件事。查询本身是只读
  // GET，不该绑在规划上。
  const fetchBtn = $('llFetch');
  if (fetchBtn && !fetchBtn.__bound) {
    fetchBtn.__bound = true;
    fetchBtn.addEventListener('click', fetchFencesAndPoints);
  }

  const ncBtn = $('netcheckBtn');
  if (ncBtn && !ncBtn.__bound) {
    ncBtn.__bound = true;
    ncBtn.addEventListener('click', runNetcheck);
  }

  if (info && !info.textContent) info.textContent = '填经纬度后点「定位」';
}

/* 网络自检：生成卡住时先点这个，确认是 Overpass 还是瓦片那条链路不通 */
async function runNetcheck() {
  const btn = $('netcheckBtn'), out = $('ncOut');
  busy(btn, true);
  out.style.display = 'block';
  out.textContent = '检测中…（Overpass 每项最多 8 秒，瓦片 6 秒）';
  try {
    const d = await api('/api/netcheck', { cache: 'no-store' });
    const lines = (d.items || []).map(it =>
      `${it.ok ? '✓' : '✗'} ${it.name}  ${it.ms}ms${it.ok ? '' : '  ' + (it.detail || '')}`);
    lines.push('');
    lines.push(d.hint || '');
    lines.push(`版本 ${d.version || '未知'}`);
    out.textContent = lines.join('\n');
  } catch (e) {
    out.textContent = '自检失败：' + ((e && e.message) || e);
  } finally {
    busy(btn, false);
  }
}

