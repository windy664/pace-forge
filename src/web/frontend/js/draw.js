/* 手绘围栏与禁区标注。加载顺序约束见 js/core.js 头注释。 */
/* ---------- 手绘轨迹 ---------- */

// 手绘状态。strokes 是若干条独立笔画，用户可能分几次画完整个圈。
let drawMode = false;
let strokes = [];
let currentStroke = null;
let drawLayer = null;
let isPointerDown = false;

// 两点间距离（米），haversine。前端只用它显示里程，精度够。
function metersBetween(a, b) {
  const R = 6371000;
  const rad = d => d * Math.PI / 180;
  const dLat = rad(b[0] - a[0]);
  const dLng = rad(b[1] - a[1]);
  const x = dLng * Math.cos(rad((a[0] + b[0]) / 2));
  return R * Math.sqrt(x * x + dLat * dLat);
}
// 橡皮：按住划过的地方，删掉半径内的手绘点。
//
// 手绘最常见的修正是「某一段画歪了 / 穿进楼里了」。整笔撤销太粗（一笔就是
// 半圈），清空重画更亏。这里只删局部点，删断的地方按段拆成独立笔画，
// 用户补一笔接回去即可。
let eraseMode = false;
let isErasing = false;
const ERASE_RADIUS_M = 12;

function eraseAt(lat, lng) {
  let touched = false;
  for (let si = strokes.length - 1; si >= 0; si--) {
    const st = strokes[si];
    const pieces = [];
    let seg = [];
    for (const q of st) {
      if (metersBetween(q, [lat, lng]) <= ERASE_RADIUS_M) {
        touched = true;
        if (seg.length >= 2) pieces.push(seg);
        seg = [];
      } else {
        seg.push(q);
      }
    }
    if (seg.length >= 2) pieces.push(seg);
    // 没改动就别动这一笔（pieces 会等于原笔，但对象不同，比较段数+首尾更稳）
    if (!(pieces.length === 1 && pieces[0].length === st.length)) {
      strokes.splice(si, 1, ...pieces);
    }
  }
  if (touched) redrawStrokes();
}

function strokesLength() {
  let m = 0;
  for (const st of strokes) {
    for (let i = 1; i < st.length; i++) m += metersBetween(st[i - 1], st[i]);
  }
  return m;
}
function redrawStrokes() {
  if (!map) return;
  if (drawLayer) map.removeLayer(drawLayer);
  drawLayer = L.featureGroup().addTo(map);
  for (const st of strokes) {
    L.polyline(st, { color: tok('--accent'), weight: 4, opacity: 0.95 }).addTo(drawLayer);
    // 起点单独标一下，用户要知道从哪开跑
    L.circleMarker(st[0], {
      radius: 5, color: tok('--warn'), fillColor: tok('--warn'), fillOpacity: 0.9, weight: 2
    }).addTo(drawLayer).bindTooltip('起点');
  }
  const m = strokesLength();
  let txt = strokes.length
    ? `${strokes.length} 笔 · ${(m / 1000).toFixed(2)} km`
    : '还没画';

  // 记分跑要求顺序经过全部打卡点，画的时候就告诉用户漏了哪个
  if (strokes.length && lastCheckpoints.length) {
    const flat = [];
    for (const st of strokes) flat.push(...st);
    let hit = 0;
    const missed = [];
    for (const c of lastCheckpoints) {
      // 手绘点是 Leaflet 给的 GCJ-02，这里也必须拿打卡点的 GCJ-02 来算距离。
      // 原来写的是 [c.lat, c.lng]（BD-09），两套坐标系混算，500 米上下的偏差
      // 会让「经过/漏掉」判反。坐标拿不到的按「漏掉」计，分母保持一致。
      const ll = coordOf(c);
      if (!ll) { missed.push(`#${c.index}`); continue; }
      let best = Infinity;
      // 点位到任一采样点的距离；采样点本身就够密（3 m 一个）
      for (const q of flat) {
        const d = metersBetween(ll, q);
        if (d < best) best = d;
      }
      if (best < 25) hit++;
      else missed.push(`#${c.index}`);
    }
    txt += ` · 点位 ${hit}/${lastCheckpoints.length}`;
    if (missed.length) txt += `（漏 ${missed.join(',')}）`;
  }
  // 目标里程 vs 实际画了多长 —— 后端只会往日志写一句，界面上看不见。
  // 画短了照样能提交，但成绩按实际里程算，等于白画；这里当场告诉用户差多少。
  if (strokes.length) {
    const targetKm = parseFloat($('rDist') && $('rDist').value);
    const drawnKm = m / 1000;
    if (Number.isFinite(targetKm) && targetKm > 0) {
      const diff = targetKm - drawnKm;
      if (diff > targetKm * 0.1) {
        txt += ` · 目标 ${targetKm.toFixed(2)} km，还差 ${(diff * 1000).toFixed(0)} m`;
      } else if (diff < -targetKm * 0.3) {
        // 画多了不会报错，但成绩只看实际里程，多出来的白跑
        txt += ` · 比目标多 ${(-diff * 1000).toFixed(0)} m（成绩按 ${drawnKm.toFixed(2)} km 算）`;
      } else {
        txt += ` · 目标 ${targetKm.toFixed(2)} km ✓`;
      }
    }
    // 首尾没接上：后端只在 1~120m 之间自动补一段，超了就不补，
    // 提交时会被判「相邻点跳变」。这条比里程差更要紧，必须当场说。
    const flatAll = [];
    for (const st of strokes) flatAll.push(...st);
    if (flatAll.length >= 2) {
      const gap = metersBetween(flatAll[0], flatAll[flatAll.length - 1]);
      if (gap > 120) {
        txt += ` · ⚠ 起点和终点差 ${gap.toFixed(0)} m，超过 120 m 不会自动接上，`;
        txt += `提交会被判跳变，建议补一笔接回去`;
      }
    }
  }
  $('drawInfo').textContent = txt;
}

function setDrawMode(on) {
  drawMode = on;
  if (!map) return;
  if (on) {
    map.dragging.disable();
    map.getContainer().style.cursor = 'crosshair';
    $('drawStart').textContent = '绘制中…';
  } else {
    map.dragging.enable();
    map.getContainer().style.cursor = '';
    $('drawStart').textContent = '继续绘制';
  }
}
function clearDrawing() {
  strokes = [];
  currentStroke = null;
  isPointerDown = false;
  redrawStrokes();
}
function useDrawing() {
  const usable = strokes.filter(s => s.length >= 2);
  if (!usable.length) { alert('先画一条线'); return null; }
  // 多笔按顺序首尾拼成一条，供后端重采样
  const merged = [];
  for (const st of usable) {
    if (merged.length && metersBetween(merged[merged.length - 1], st[0]) < 30) {
      merged.push(...st.slice(1));
    } else {
      merged.push(...st);
    }
  }
  return merged;
}

/* ---------- 禁区标注 ----------
 * OSM 在国内校园基本是空白（实测这个校区整个 bbox 只有 15 个 way、76 个
 * node，一栋楼都没有），Overpass 拉不到建筑轮廓，轨迹就只能按几何直线走，
 * 从教学楼、宿舍楼中间穿过去。
 *
 * 这里让人在地图上圈一次：拖出矩形盖住楼，存到本地 blockers.json，
 * 之后所有规划都会绕开（fence_route::avoid_blockers）。
 */
let blockMode = false, blockStart = null, blockTemp = null, localBlockers = [];
let blockLayer = null;

function blockerRectFromBounds(b) {
  return [
    [b.getSouth(), b.getWest()],
    [b.getSouth(), b.getEast()],
    [b.getNorth(), b.getEast()],
    [b.getNorth(), b.getWest()],
  ];
}

function drawBlockers() {
  if (!blockLayer) return;
  blockLayer.clearLayers();
  localBlockers.forEach(poly => {
    L.polygon(poly, { color: tok('--bad'), weight: 2, dashArray: '5 3', fillOpacity: .18 })
      .addTo(blockLayer);
  });
  const n = localBlockers.length;
  $('blockInfo').textContent = blockMode
    ? (n ? `拖拽继续圈，共 ${n} 块；画完点「保存禁区」` : '在地图上按住拖出一个矩形，把楼盖住')
    : (n ? `已标注 ${n} 块禁区（轨迹会绕开）` : '还没标注禁区');
}

function setBlockMode(v) {
  blockMode = v;
  $('blockStart').textContent = v ? '停止标注' : '标注禁区（画楼）';
  if (map) map.getContainer().style.cursor = v ? 'crosshair' : '';
  if (!v && blockTemp) { blockTemp.remove(); blockTemp = null; }
  blockStart = null;
  drawBlockers();
}

async function loadBlockers() {
  try {
    const r = await api('/api/blockers');
    localBlockers = (r.polys || []).map(p => p.map(q => [q[0], q[1]]));
    drawBlockers();
  } catch (e) {
    $('blockInfo').textContent = '读取已存禁区失败：' + e.message;
  }
}

function initBlockers() {
  if (!map || map.__blockBound) return;
  map.__blockBound = true;
  blockLayer = L.layerGroup().addTo(map);
  $('blockStart').onclick = () => setBlockMode(!blockMode);
  $('blockUndo').onclick = () => {
    if (!localBlockers.length) return;
    localBlockers.pop();
    drawBlockers();
  };
  $('blockSave').onclick = async () => {
    const btn = $('blockSave');
    if (btn.disabled) return;
    // 打卡点回钉会撤销避让（`pin_checkpoints` 无条件把点插回路径），所以
    // 一块盖住打卡点的禁区等于对那个点无效——绕出去又被钉回来。审计链是
    // 「先推再钉」三步互相打架，更彻底的做法是路图求解时就把必经点当输入，
    // 做不到就直接报错，不留静默穿墙。
    // 这里至少在保存那一刻就把话说清楚，别等生成完才发现墙上有个洞。
    if (lastCheckpoints.length && localBlockers.length) {
      const covered = [];
      lastCheckpoints.forEach((c, i) => {
        const ll = coordOf(c);
        if (!ll) return;
        if (localBlockers.some(poly => pointInPoly(poly, ll[0], ll[1]))) {
          const name = (c.pointName || c.name || '').trim();
          covered.push(name ? `#${i + 1} ${name}` : `#${i + 1}`);
        }
      });
      if (covered.length && !confirm(
        `${covered.length} 个打卡点落在你要保存的禁区里：\n\n${covered.join('\n')}\n\n` +
        `这些点必须经过（服务端按轨迹是否到过点位判定），而绕行会被打卡点回钉撤销——` +
        `所以这几块禁区对它们无效，轨迹仍会穿过去。\n\n` +
        `想真正绕开就把禁区缩小、或者挪开不要压住打卡点。\n\n仍然保存？`
      )) return;
    }
    busy(btn, true);
    try {
      const r = await api('/api/blockers', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ polys: localBlockers }),
      });
      $('blockInfo').textContent = `已保存 ${r.saved} 块禁区，之后的轨迹会绕开`;
      setBlockMode(false);
    } catch (e) {
      $('blockInfo').textContent = '保存失败：' + e.message;
    } finally {
      busy(btn, false);
    }
  };
  loadBlockers();
}

function initDrawing() {
  if (!map || map.__drawBound) return;
  map.__drawBound = true;
  const el = map.getContainer();

  const toLatLng = ev => {
    const r = el.getBoundingClientRect();
    return map.containerPointToLatLng(L.point(ev.clientX - r.left, ev.clientY - r.top));
  };

  el.addEventListener('pointerdown', ev => {
    if (blockMode) {
      ev.preventDefault();
      blockStart = toLatLng(ev);
      blockTemp = L.rectangle(L.latLngBounds(blockStart, blockStart), {
        color: '#f85149', weight: 2, fillOpacity: .2,
      }).addTo(map);
      return;
    }
    if (!drawMode) return;
    if (eraseMode) {
      ev.preventDefault();
      isErasing = true;
      try { el.setPointerCapture(ev.pointerId); } catch (e) {}
      const p = toLatLng(ev);
      eraseAt(p.lat, p.lng);
      return;
    }
    ev.preventDefault();
    isPointerDown = true;
    el.setPointerCapture(ev.pointerId);
    const ll = toLatLng(ev);
    currentStroke = [[ll.lat, ll.lng]];
    strokes.push(currentStroke);
    redrawStrokes();
  });

  el.addEventListener('pointermove', ev => {
    if (blockMode && blockStart && blockTemp) {
      blockTemp.setBounds(L.latLngBounds(blockStart, toLatLng(ev)));
      return;
    }
    if (isErasing) {
      ev.preventDefault();
      const p = toLatLng(ev);
      eraseAt(p.lat, p.lng);
      return;
    }
    if (!drawMode || !isPointerDown || !currentStroke) return;
    ev.preventDefault();
    const ll = toLatLng(ev);
    const last = currentStroke[currentStroke.length - 1];
    // 太密的点丢掉，不然一条线几千个点，报文会爆
    if (metersBetween(last, [ll.lat, ll.lng]) < 3) return;
    currentStroke.push([ll.lat, ll.lng]);
    redrawStrokes();
  });

  const finish = ev => {
    if (blockMode && blockStart && blockTemp) {
      const b = blockTemp.getBounds();
      blockTemp.remove();
      blockTemp = null;
      blockStart = null;
      // 太小的当成误点，不记
      const dN = metersBetween([b.getSouth(), b.getWest()], [b.getNorth(), b.getEast()]);
      if (dN >= 8) {
        localBlockers.push(blockerRectFromBounds(b));
        drawBlockers();
      }
      return;
    }
    if (isErasing) {
      isErasing = false;
      try { el.releasePointerCapture(ev.pointerId); } catch (e) {}
      redrawStrokes();
      return;
    }
    if (!isPointerDown) return;
    isPointerDown = false;
    currentStroke = null;
    try { el.releasePointerCapture(ev.pointerId); } catch (e) {}
    redrawStrokes();
  };
  el.addEventListener('pointerup', finish);
  el.addEventListener('pointercancel', finish);
  el.addEventListener('pointerleave', finish);

  $('drawStart').onclick = () => setDrawMode(!drawMode);
  $('drawUndo').onclick = () => { strokes.pop(); redrawStrokes(); };
  $('drawErase').onclick = () => {
    eraseMode = !eraseMode;
    $('drawErase').textContent = eraseMode ? '橡皮（开）' : '橡皮';
    // 没有现成的按钮激活类，直接用强调色标一下，省得改一套 CSS
    $('drawErase').style.color = eraseMode ? 'var(--accent)' : '';
    $('drawErase').style.borderColor = eraseMode ? 'var(--accent)' : '';
    setOut(eraseMode ? '橡皮已开：按住划过要删的那一段（12m 内）' : '橡皮已关，回到绘制');
  };
  $('drawClear').onclick = clearDrawing;
  $('drawUse').onclick = () => {
    const merged = useDrawing();
    if (!merged) return;
    setDrawMode(false);
    $('drawInfo').textContent =
      `已采用 ${(strokesLength() / 1000).toFixed(2)} km，点“生成轨迹”或直接提交`;
  };
}

/* 两个轨迹开关：勾上立刻上图，取消立刻摘下来，不用重新规划 */
document.addEventListener('change', (e) => {
  const id = e.target && e.target.id;
  if (id === 'showTrack' && trackLayer) {
    e.target.checked ? trackLayer.addTo(map) : map.removeLayer(trackLayer);
  }
  if (id === 'showRec' && layer) {
    e.target.checked ? layer.addTo(map) : map.removeLayer(layer);
  }
});

