/* 真跑生成、账号管理、起点时间。加载顺序约束见 js/core.js 头注释。 */
/* ---------- 真跑 ---------- */
$('runBtn').onclick = async () => {
  const dist = parseFloat($('rDist').value);
  const dur = readDur(dist);
  if (!(dist > 0) || !(dur > 0)) return setOut('距离和时长要填正数', true);
  const mode = parseInt($('rRunMode').value, 10) || 1;
  const modeName = {1: '记分跑', 4: '户外跑'}[mode] || '记分跑';
  let st = null;
  try { st = await resolveStartAt(); } catch (e) { if (e.message !== '已取消') setOut(e.message, true); return; }
  const when = st ? `${st.startText} ~ ${st.stopText}` : '现在开始';
  if (!confirm(`确认提交 ${modeName}：${dist} km / ${dur} 分钟？\n时间：${when}\n\n提交后 10 分钟内不能再跑。`)) return;

  const btn = $('runBtn');
  busy(btn, true);
  setOut('提交中，别关页面…');
  try {
    const r = await api('/api/run', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        distance: dist, duration: dur,
        // 户外跑（自由跑）走 runMode=2 策略（无时段限制）；记分跑 runMode=1。
        runMode: mode === 1 ? 1 : 2,
        // 跑法选什么就发什么：1 记分跑 / 4 户外跑（自由跑，实测同一类）。
        sportType: mode,
        mode: $('rMode').value,
        lane: parseInt($('rLane').value, 10) || 1,
        preferRoads: !$('rNoRoads').checked,
        ...altBody(),
        startAtMs: (st ? st.startMs : undefined),
        ...locBody(),
        ...endBody(),
        // 手绘模式把坐标带上；后端按 WGS84 解析并转 BD-09
        customPath: $('rMode').value === 'draw'
          ? (() => {
              const m = useDrawing();
              return m && m.length >= 2 ? m : undefined;
            })()
          : undefined
      })
    });
    setOut('');
    $('planOut').innerHTML = `
      <div class="msg ok" style="font-size:15px;font-weight:600">
        ${r.submitted ? '✓ 提交成功' : '未提交'}</div>
      <div class="kv"><span>记录 id</span><b>${esc(r.recordId ?? '—')}</b></div>
      <div class="kv"><span>里程</span><b>${r.distance} m</b></div>
      <div class="kv"><span>步数</span><b>${r.steps}</b></div>
      <div class="kv"><span>步频</span><b>${(r.cadence||0).toFixed(1)} 步/分</b></div>
      <div class="kv"><span>明细 key</span><b>${r.obsKeys || 0} 个已上传</b></div>
      ${r.logs && r.logs.length ? `<details style="margin-top:8px"><summary style="cursor:pointer;color:var(--dim)">流程日志</summary><pre class="mono" style="font-size:11px;max-height:240px;overflow:auto;white-space:pre-wrap">${esc(r.logs.join('\n'))}</pre></details>` : ''}`;
    loadRecords();
  } catch (e) {
    setOut(e.message, true);
    $('planOut').innerHTML = `<div class="msg err">提交失败</div><pre class="mono" style="font-size:11px;white-space:pre-wrap;max-height:300px;overflow:auto">${esc(e.message)}</pre>`;
  } finally {
    busy(btn, false);
  }
};

function setOut(t, err) {
  const o = $('planOut');
  // 入参都是纯文本（静态提示或异常文案），在这里统一转义，避免调用方漏包
  o.innerHTML = t ? `<div class="msg ${err ? 'err' : ''}">${esc(t)}</div>` : '';
}
let lastCheckpoints = [];
let lastFence = null;
// 预览轨迹单独一层，默认不上图（见 drawPreview）
let trackLayer = null;

/// 围栏环 → 底图坐标（GCJ-02）。
///
/// 后端 `Fence.ring` 统一是 **BD-09**（服务端下发 lat/lng 时按百度系解释，
/// 只有 `glat/glon` 那套会先转 BD-09）。而这里铺的是高德瓦片，Leaflet 的
/// latlng 数值就是 GCJ-02——直接把 BD-09 塞进去，整圈围栏会偏出去几百米，
/// 看着像「围栏飘了」。所以后端两个接口都额外给了 `ringGcj` /
/// `allPolygonsGcj`，这里优先用；老响应没有这个字段时才退回 `ring`
/// （那时会偏，但至少画得出来）。
function fenceRing(f) {
  if (!f) return [];
  if (Array.isArray(f.ringGcj) && f.ringGcj.length) return f.ringGcj;
  return f.ring || [];
}

/// 围栏是否已经从 BD-09 转到了底图坐标。用来在界面上区分
/// 「偏是因为数据没转」和「偏是因为围栏本身就这样」。
function fenceIsGcj(f) {
  return !!(f && Array.isArray(f.ringGcj) && f.ringGcj.length);
}

  // ── 网络出口（代理） ──────────────────────────────────────────────
  // 保存后重建所有 client，提交链路下一次请求就走新出口；探测接口本身仍直连。
  async function egLoad() {
    try {
      const d = await api('/api/egress-ip');
      $('egIp').textContent = d.ok ? `${d.ip}（${d.source}）` : '未知';
      $('egProxy').textContent = d.proxy || '未设置（直连）';
      $('egProxy').className = d.proxy ? 'ok' : 'hint';
    } catch (e) {
      $('egIp').textContent = '读取失败';
    }
  }
  async function egSave(proxy) {
    try {
      const d = await api('/api/egress', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ proxy })
      });
      $('egMsg').className = 'msg ok';
      $('egMsg').textContent = `${d.note}（重建 ${d.rebuilt} 个会话）`;
      await egLoad();
    } catch (e) {
      $('egMsg').className = 'msg err';
      $('egMsg').textContent = '保存失败：' + e.message;
    }
  }
  if ($('egInput')) {
    $('egSave').onclick = () => {
      const v = $('egInput').value.trim();
      $('egMsg').textContent = '';
      egSave(v || null);
    };
    $('egProbe').onclick = () => { egLoad(); };
    egLoad();
  }

  // ── 设备机型（按品牌分组的两个下拉） ──────────────────────────────
  // 品牌 → 机型 两级联动，平台单独一个下拉（内置 17 条）。
  // 只改 cpuModel，不动 device_id；映射按账号落在本机。
  const DEV = { brands: [], platforms: [], model: '' };
  function devRender(d) {
    DEV.model = d.model || '';
    $('devCpu').textContent = d.cpuModel || 'arm64-v8a';
    const hit = d.catalogHit;
    $('devSrc').textContent = d.model
      ? `${d.model}${hit ? ` · ${hit.brand} ${hit.name}` : '（官方目录未收录）'}`
      : '未设置（ABI 兜底）';
    $('devSrc').className = d.abiFallback ? 'hint' : 'ok';
  }
  function devFillSelect(el, placeholder, items, labelOf, valueOf) {
    el.innerHTML = '';
    el.appendChild(new Option(placeholder, ''));
    for (const it of items) el.appendChild(new Option(labelOf(it), valueOf(it)));
  }
  async function devInit() {
    try {
      const d = await api('/api/device');
      DEV.brands = d.brands || [];
      DEV.platforms = d.platforms || [];
      devRender(d);
      devFillSelect($('devBrand'), '品牌…', DEV.brands,
        b => `${b.name}（${b.count}）`, b => b.name);
      devFillSelect($('devPlatformSel'), '平台（可选）', DEV.platforms,
        p => `${p.name}（${p.code} → ${p.soc}）`, p => p.code);
      // 当前机型所属品牌预选，省得每次重新找
      if (d.catalogHit) {
        const opt = [...$('devBrand').options].find(o => o.value.toLowerCase() === String(d.catalogHit.brand).toLowerCase());
        if (opt) { $('devBrand').value = opt.value; await devPickBrand(opt.value); }
      }
    } catch (e) {
      $('devMsg').className = 'msg err';
      $('devMsg').textContent = '读取机型列表失败：' + e.message;
    }
  }
  async function devPickBrand(brand) {
    const sel = $('devModel');
    if (!brand) { devFillSelect(sel, '先选品牌', [], () => '', () => ''); return; }
    sel.innerHTML = '';
    sel.appendChild(new Option('加载中…', ''));
    try {
      const d = await api('/api/device?brand=' + encodeURIComponent(brand));
      devFillSelect(sel, '机型…', d.results || [],
        r => `${r.name} · ${r.model}`, r => r.model);
    } catch (e) {
      devFillSelect(sel, '读取失败', [], () => '', () => '');
      $('devMsg').className = 'msg err';
      $('devMsg').textContent = e.message;
    }
  }
  async function devSave(model, platform) {
    try {
      const d = await api('/api/device', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ model, platform: platform || undefined })
      });
      devRender(d);
      $('devMsg').className = 'msg ok';
      $('devMsg').textContent = d.note || '已保存';
    } catch (e) {
      $('devMsg').className = 'msg err';
      $('devMsg').textContent = '保存失败：' + e.message;
    }
  }
  if ($('devBrand')) {
    // 搜索直接用目录检索，输什么都在列表里出现；清空就回到按品牌浏览。
    let devTimer = null;
    $('devSearch').oninput = () => {
      clearTimeout(devTimer);
      const q = $('devSearch').value.trim();
      devTimer = setTimeout(async () => {
        if (!q) { await devPickBrand($('devBrand').value); return; }
        try {
          const d = await api('/api/device?q=' + encodeURIComponent(q));
          devFillSelect($('devModel'), '搜索结果', d.results || [],
            r => `${r.name} · ${r.model}`, r => r.model);
        } catch (e) {
          $('devMsg').className = 'msg err';
          $('devMsg').textContent = e.message;
        }
      }, 300);
    };
    $('devBrand').onchange = () => { $('devSearch').value = ''; devPickBrand($('devBrand').value); };
    $('devApply').onclick = () => {
      const model = $('devModel').value;
      if (!model) { $('devMsg').className = 'msg err'; $('devMsg').textContent = '先选一个机型'; return; }
      devSave(model, $('devPlatformSel').value);
    };
    $('devClear').onclick = () => {
      $('devMsg').textContent = '';
      devSave(null);
    };
    devInit();
  }

function drawPreview(coords, fence, checkpoints) {
  if (!map) return;
  // 后端在手绘、操场、部分预览分支返回的是 `checkpoints: []`——空数组在 JS 里
  // 是 truthy，原来这句会把「查围栏/点位」已经拿到的点位整个覆盖成空，
  // 图上点位全消失、手绘面板的「点位 N/M」也归零。只有非空点集才算新状态。
  if (Array.isArray(checkpoints) && checkpoints.length) lastCheckpoints = checkpoints;
  const cps = lastCheckpoints;
  if (fence) lastFence = fence;
  if (previewLayer) { map.removeLayer(previewLayer); previewLayer = null; }

  const group = L.featureGroup();

  // 围栏
  const fring = fenceRing(fence);
  if (fring.length >= 3) {
    const poly = L.polygon(fring, {
      color: tok('--warn'), weight: 2, fillOpacity: 0.05, dashArray: '6 4'
    }).bindPopup('围栏：' + esc(fence.name) +
      (fenceIsGcj(fence) ? '' : '<br><span style="color:var(--danger)">坐标系未转换，可能整体偏移</span>'));
    group.addLayer(poly);
  }

  // 轨迹单独一层，默认不显示。
  // 原来它跟围栏、打卡点挤在同一个 group 里，规划完就一直挂着，切记录也
  // 不消失，叠两三条就看不见底下的路。
  if (trackLayer) { map.removeLayer(trackLayer); trackLayer = null; }
  if (coords && coords.length) {
    trackLayer = L.layerGroup();
    L.polyline(coords, { color: tok('--accent'), weight: 4, opacity: 0.9 }).addTo(trackLayer);
    // 生成成功就把线亮出来：这个复选框默认不勾，用户点完「生成轨迹」只看到
    // 自检通过、地图上一片空白，会以为生成失败（#2 报的就是这个）。想隐藏
    // 再取消勾选即可，下一次生成会重新亮。
    const cbTrack = $('showTrack');
    if (cbTrack && !cbTrack.checked) cbTrack.checked = true;
    trackLayer.addTo(map);
  }

  // 打卡点
  if (cps && cps.length) {
    for (const c of cps) {
    
  // 高德瓦片是 GCJ-02 网格，所以打点要用 gcjLat/gcjLng（/api/plan 两个
      // 都给了：lat/lng 是 BD-09，gcjLat/gcjLng 是 GCJ-02）。字段名与有效性
      // 统一交给 coordOf，取不到有效坐标就跳过，不拿占位值往图上画。
      const ll = coordOf(c);
      if (!ll) continue;
      // passed 三态：true 经过、false 没经过、null 还没生成轨迹（「查围栏/点位」
      // 那条路进来时没有轨迹可比）。把 null 说成「未经过」是错的判断，
      // 用户会以为点位被算法漏掉了。
      const known = c.passed === true || c.passed === false;
      const color = c.passed === true ? tok('--ok') : (c.passed === false ? tok('--danger') : tok('--dim'));
      const state = c.passed === true ? '✓ 轨迹经过'
        : (c.passed === false ? `✗ 未经过（最近 ${c.nearest}m）` : '未生成轨迹，无法判断是否经过');
      group.addLayer(L.circleMarker(ll, {
        radius: 7, color: color, fillColor: color, fillOpacity: 0.85, weight: 2,
        // 没有轨迹时用虚线圈，跟「已判定」的点在图上区分开
        dashArray: known ? null : '3 3',
      }).bindPopup(
        `打卡点 #${c.index}${c.name ? ' ' + esc(c.name) : ''}<br>${state}`
      ));
    }
  }

  // 用户手填的中心点单独标出来。
  //
  // 规划器会往打卡点靠，所以轨迹中心常常离填的坐标有几百米。只画轨迹的话
  // 用户会觉得「坐标没生效」——把锚点也画上，一眼就能看出是「锚点在这、
  // 轨迹在那」，而不是「我的输入被忽略了」。
  const anchor = readLocInputs();
  if (anchor) {
    const ag = toGcj02(anchor.lat, anchor.lng, anchor.coord);
    group.addLayer(L.circleMarker(ag, {
      radius: 9, color: tok('--fg'), fillColor: tok('--fg'), fillOpacity: .18, weight: 3,
    }).bindPopup('你指定的中心点（' + anchor.coord.toUpperCase() + ' 数值）'));
  }

  previewLayer = group.addTo(map);
  try {
    const b = group.getBounds();
    if (b.isValid()) map.fitBounds(b, { padding: [30, 30] });
  } catch (e) {}
}


/* ---------- 账号管理 ---------- */

async function loadAccounts() {
  try {
    const r = await api('/api/accounts');
    renderAccounts(r);
    return r;
  } catch (e) {
    $('accList').innerHTML = `<div class="msg err">${esc(e.message)}</div>`;
  }
}

function renderAccounts(r) {
  $('accCount').textContent = `${r.count} 个 · ${r.ready} 个就绪`;
  const list = $('accList');
  if (!r.accounts || !r.accounts.length) {
    list.innerHTML = '<div class="empty">还没有账号 · 在下面填手机号和密码，点「添加」</div>';
    return;
  }
  list.innerHTML = r.accounts.map(a => {
    const dot = a.loggedIn ? 'var(--ok)' : (a.enabled ? 'var(--warn)' : 'var(--dim)');
    const state = a.loggedIn ? '已登录'
      : (a.hasPassword ? (a.enabled ? '待登录' : '已停用') : '无密码');
    return `<div class="accrow" data-id="${esc(a.id)}" style="display:flex;align-items:center;gap:10px;padding:8px 0;border-bottom:1px solid var(--line-soft)${a.current ? ';background:var(--panel2);border-radius:6px;padding-left:8px' : ''}">
      <span style="width:8px;height:8px;border-radius:50%;background:${dot};flex:none"></span>
      <div class="accname" style="flex:1;min-width:0">
        <b style="font-size:13px">${esc(a.label)}${a.current ? ' <span style="color:var(--accent);font-size:11px">当前</span>' : ''}</b>
        <div class="hint" style="font-size:11px">${esc(a.username)} · ${esc(a.school || '—')} · ${state}</div>
      </div>
      <button class="ghost mini" data-act="switch"  data-id="${esc(a.id)}" ${a.current ? 'disabled' : ''}>切换</button>
      <button class="ghost mini" data-act="login"   data-id="${esc(a.id)}" ${a.hasPassword ? '' : 'disabled'}>重登</button>
      <button class="ghost mini" data-act="logout"  data-id="${esc(a.id)}" ${a.loggedIn ? '' : 'disabled'}>退出</button>
      <button class="ghost mini" data-act="toggle"  data-id="${esc(a.id)}">${a.enabled ? '停用' : '启用'}</button>
      <button class="ghost mini" data-act="del"     data-id="${esc(a.id)}" style="color:var(--danger)">删除</button>
    </div>`;
  }).join('');

  list.querySelectorAll('button[data-act]').forEach(b => {
    b.onclick = () => accountAction(b.dataset.act, b.dataset.id);
  });
}

/* 账号写操作进行中标记：一次请求没结算前，连点其余写按钮直接忽略，
   避免切换/重登/退出/停用/删除并发打到后端。 */
let accActionBusy = false;
async function accountAction(act, id) {
  if (accActionBusy) return;
  accActionBusy = true;
  // 记下每个按钮原本的可用状态：失败或用户取消时按原样恢复，
  // 不能一律恢复成可用（「退出」对未登录账号本来就该是禁用的）。
  const btns = Array.from(document.querySelectorAll('#accList button[data-act]'));
  const prevDisabled = btns.map(b => b.disabled);
  btns.forEach(b => { b.disabled = true; });
  const m = $('accMsg');
  try {
    if (act === 'del') {
      if (!confirm('删除这个账号？\n\n会先退出登录再删（不然服务端会话留活，下次登要锁一小时），存盘的密码也会一起删掉。\n\n只是想换账号的话，用旁边的「退出」——密码留着，随时重登。')) return;
      m.textContent = '退出登录中…';
      const url = '/api/accounts/' + encodeURIComponent(id);
      let r = await api(url, { method: 'DELETE' });
      // 服务端没确认退出时后端不会真删，先让用户知道后果再决定
      if (r.needConfirm) {
        if (!confirm((r.warning || '服务端未确认退出登录。') + '\n\n仍要删除这个账号？')) {
          m.textContent = '已取消删除，账号还在';
          m.className = 'msg';
          return;
        }
        r = await api(url + '?force=1', { method: 'DELETE' });
      }
      m.textContent = r.remoteLogout
        ? '已退出登录并删除'
        : (r.removed ? '已删除（服务端未确认退出，建议半小时内别重登）' : '已取消删除');
    } else if (act === 'switch') {
      await api('/api/accounts/' + encodeURIComponent(id) + '/switch', { method: 'POST' });
      m.textContent = '已切换';
      await boot();
    } else if (act === 'login') {
      m.textContent = '登录中…';
      const r = await api('/api/accounts/' + encodeURIComponent(id) + '/login', { method: 'POST' });
      m.textContent = '登录成功：' + (r.name || r.uid);
    } else if (act === 'logout') {
      // 退出登录：通知服务端让 token 失效，本地只清登录态，密码留着。
      // 不走这一步直接删账号的话，服务端会话还活着，账号仍被标记「已登录」，
      // 下次登录触发 10121 并锁一小时。
      const r = await api('/api/accounts/' + encodeURIComponent(id) + '/logout', { method: 'POST' });
      m.textContent = r.remoteLogout
        ? '已退出登录，密码已保留，随时可以重登'
        : '本地已退出，服务端未确认（半小时内别重登）';
    } else if (act === 'toggle') {
      await api('/api/accounts/' + encodeURIComponent(id) + '/toggle', { method: 'POST' });
      m.textContent = '';
    }
    m.className = 'msg';
    await loadAccounts();
  } catch (e) {
    m.textContent = e.message;
    m.className = 'msg err';
  } finally {
    // 成功路径已经由 loadAccounts() 重渲染出新按钮，这里的旧按钮已脱落，
    // 恢复它们无副作用；失败/取消路径则靠这一步把按钮放回原状态。
    btns.forEach((b, i) => { b.disabled = prevDisabled[i]; });
    accActionBusy = false;
  }
}

/* ---------- 何时跑 ---------- */
$('rWhen').onchange = () => {
  const v = $('rWhen').value;
  $('wrapAt').style.display   = (v === 'at') ? '' : 'none';
  $('wrapDays').style.display = (v === 'at' || v === 'rand') ? '' : 'none';
  $('wrapAgo').style.display  = (v === 'ago') ? '' : 'none';
};

/** 问后端要补跑时刻。返回 {startMs, startText, stopText, clamped} 或 null（现在跑）。 */
async function resolveStartAt() {
  const mode = $('rWhen').value;
  if (mode === 'now') return null;
  const dur = Math.round(parseFloat($('rDur').value) * 60) || 720;
  const qs = new URLSearchParams({ mode, dur: String(dur) });
  if (mode === 'at') {
    qs.set('hhmm', $('rAt').value || '07:30');
    qs.set('days', $('rDays').value || '0');
  } else if (mode === 'ago') {
    qs.set('minutes', $('rAgo').value || '60');
  } else if (mode === 'rand') {
    qs.set('days', $('rDays').value || '0');
  }
  const r = await api('/api/starttimes?' + qs.toString());
  if (r.clamped) {
    if (!confirm(`你选的时间会让结束时刻落在未来。\n\n已自动提前到：${r.startText}\n结束：${r.stopText}\n\n继续？`)) {
      throw new Error('已取消');
    }
  }
  return r;
}

$('accAdd').onclick = async () => {
  const u = $('accUser').value.trim();
  const p = $('accPass').value;
  const m = $('accMsg');
  if (!u || !p) { m.textContent = '手机号和密码都要填'; m.className = 'msg err'; return; }
  const btn = $('accAdd');
  busy(btn, true);
  m.textContent = '登录中（极验本地求解，可能要几秒）…';
  m.className = 'msg';
  try {
    const r = await api('/api/accounts', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: u, password: p })
    });
    m.textContent = `已添加：${r.name || r.uid}`;
    $('accUser').value = '';
    $('accPass').value = '';
    await loadAccounts();
    await boot();
  } catch (e) {
    m.textContent = e.message;
    m.className = 'msg err';
  } finally {
    busy(btn, false);
  }
};

$('accLoginAll').onclick = async () => {
  const m = $('accMsg');
  const btn = $('accLoginAll');
  busy(btn, true);
  m.textContent = '批量登录中…';
  m.className = 'msg';
  try {
    const r = await api('/api/accounts/login-all', { method: 'POST' });
    m.textContent = `完成：${r.ok}/${r.total} 个成功`;
    const bad = (r.results || []).filter(x => !x.ok);
    if (bad.length) {
      m.textContent += ' · 失败：' + bad.map(x => x.error).slice(0, 3).join('；');
      m.className = 'msg warn';
    }
    await loadAccounts();
  } catch (e) {
    m.textContent = e.message;
    m.className = 'msg err';
  } finally {
    busy(btn, false);
  }
};

$('accBatch').onclick = async () => {
  const dist = parseFloat($('rDist').value);
  const dur = readDur(dist);
  const mode = parseInt($('rRunMode').value, 10) || 1;
  const modeName = {1: '记分跑', 4: '户外跑'}[mode] || '记分跑';
  if (!(dist > 0) || !(dur > 0)) { $('accMsg').textContent = '先在上面的表单填好距离和时长'; return; }
  let st = null;
  try { st = await resolveStartAt(); } catch (e) { if (e.message !== '已取消') return; }
  const when = st ? `${st.startText} ~ ${st.stopText}` : '现在开始';
  if (!confirm(`所有已登录账号都跑 ${dist} km / ${dur} 分钟（${modeName}）？\n时间：${when}\n\n账号之间间隔 8 秒，避免同 IP 高频。`)) return;

  const m = $('accMsg');
  const btn = $('accBatch');
  busy(btn, true);
  m.textContent = '批量提交中，别关页面…';
  m.className = 'msg';
  try {
    const r = await api('/api/batch', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        distance: dist, duration: dur,
        runMode: mode === 1 ? 1 : 2,
        // 跑法选什么就发什么：1 记分跑 / 4 户外跑（自由跑，实测同一类）。
        sportType: mode, gapMs: 8000,
        ...altBody(),
        startAtMs: (st ? st.startMs : undefined)
      })
    });
    const lines = (r.results || []).map(x => {
      if (x.ok && x.submitted) return `✓ ${esc(x.id)} → 记录 #${esc(x.recordId)}`;
      if (x.ok) return `· ${esc(x.id)} 未提交`;
      return `✗ ${esc(x.id)} → ${esc(x.error)}`;
    });
    m.innerHTML = `<b>完成：${r.submitted}/${r.total} 个已提交</b><pre class="mono" style="font-size:11px;white-space:pre-wrap;max-height:240px;overflow:auto;margin-top:6px">${lines.join('\n')}</pre>`;
    m.className = r.submitted === r.total ? 'msg ok' : 'msg warn';
    await loadAccounts();
  } catch (e) {
    m.textContent = e.message;
    m.className = 'msg err';
  } finally {
    busy(btn, false);
  }
};

