/* 启动入口：登录态判定、控制台解锁、账号选择与 boot()。加载顺序约束见 js/core.js 头注释。 */
/* ---------- 启动 ---------- */
function showLogin() {
  $('app').style.display = 'none';
  $('picker').style.display = 'none';
  $('login').style.display = '';
}

function showConsoleUnlock(rejected) {
  showLogin();
  $('consoleMsg').textContent = rejected ? '控制台口令不正确，请重试' : '';
  rememberConsoleToken('');
  $('accountLogin').hidden = true;
  $('consoleUnlock').hidden = false;
  $('consoleToken').focus();
}

async function boot(candidate) {
  let me;
  try {
    me = await api('/api/me', candidate ? { headers: { 'X-Pace-Token': candidate } } : undefined);
  } catch (e) {
    // Only the console password failure is 401 + console_auth; a vendor session
    // expiring is 401 + auth and must show its message instead of the unlock box.
 {
    if (e.status === 401 && e.kind === 'console_auth') {
      showConsoleUnlock(candidate || consoleToken);
      return;
    }
    if (e.status !== 200) {
      if (candidate) {
        $('consoleMsg').textContent = e.message;
      } else {
        showLogin();
        setMsg(e.message, 'err');
      }
      return;
    }
  }
  if (candidate) rememberConsoleToken(candidate);
  $('consoleUnlock').hidden = true;
  $('accountLogin').hidden = false;
  if (me) {
    $('picker').style.display = 'none';
    enter(me);
    await loadAccounts();
    return;
  }
  // 未登录时优先显示已有账号的选择界面。
  showLogin();
  try {
    const r = await api('/api/accounts');
    const usable = (r.accounts || []).filter(a => a.hasPassword);
    if (usable.length) showPicker(usable, r);
  } catch (e) {
    if (e.status === 401 && e.kind === 'console_auth') showConsoleUnlock(consoleToken);
    else setMsg(e.message, 'err');
  }
}

$('consoleUnlock').onsubmit = async (e) => {
  e.preventDefault();
  const input = $('consoleToken');
  const btn = $('consoleUnlockBtn');
  if (!input.value || btn.disabled) return;
  const candidate = input.value;
  input.value = '';
  $('consoleMsg').textContent = '';
  btn.disabled = true;
  try {
    await boot(candidate);
  } finally {
    btn.disabled = false;
  }
};

function showPicker(list, r) {
  $('login').style.display = 'none';
  const box = $('picker');
  box.style.display = 'flex';
  box.innerHTML = `
    <div class="pick-card">
      <h1>选择账号</h1>
      <p class="sub">库里存了 ${list.length} 个账号，点一下直接登录</p>
      ${list.map(a => `
        <button class="ghost pick-item" data-id="${esc(a.id)}">
          <b>${esc(a.label)}</b>
          <span class="sub2">${esc(a.username)} · ${esc(a.school || '—')}</span>
        </button>`).join('')}
      <button class="ghost" id="pickOther" style="width:100%;margin-top:6px">用其他账号登录</button>
      <div class="msg" id="pickMsg"></div>
    </div>`;

  box.querySelectorAll('button[data-id]').forEach(b => {
    b.onclick = async () => {
      if (b.disabled) return;
      // 登录请求没结算前禁用本卡片所有账号按钮，避免双击/连点重复提交
      box.querySelectorAll('button[data-id]').forEach(x => { x.disabled = true; });
      const m = $('pickMsg');
      m.textContent = '登录中…';
      m.className = 'msg';
      try {
        const res = await api('/api/accounts/' + encodeURIComponent(b.dataset.id) + '/login', { method: 'POST' });
        box.style.display = 'none';
        $('picker').innerHTML = '';
        enter({ name: res.name, uid: res.uid });
        await loadAccounts();
      } catch (e) {
        m.textContent = e.message;
        m.className = 'msg err';
        box.querySelectorAll('button[data-id]').forEach(x => { x.disabled = false; });
      }
    };
  });
  $('pickOther').onclick = () => {
    box.style.display = 'none';
    box.innerHTML = '';
    $('login').style.display = 'flex';
  };
}

boot();
