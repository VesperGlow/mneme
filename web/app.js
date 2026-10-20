'use strict';
(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
  const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
  const kinds = { fact: '事实', preference: '偏好', event: '经历', relationship: '关系', emotion: '情绪' };
  const colors = { fact: '#73906c', preference: '#c7ac7d', event: '#9aaec3', relationship: '#b4a0bb', emotion: '#c49b92' };
  const pages = { chat: ['对话'], memories: ['记忆库'], agent: ['Agent'], tools: ['工具记录'], retrieval: ['检索实验室'], system: ['系统状态'], settings: ['设置'] };
  const paths = {
    grid: '<rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/>',
    book: '<path d="M4 4h6a3 3 0 0 1 3 3v14a4 4 0 0 0-4-2H4z"/><path d="M13 7a3 3 0 0 1 3-3h5v15h-4a4 4 0 0 0-4 2"/>',
    search: '<circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 4.5 4.5"/>',
    system: '<path d="m3 13 4-1 3-8 4 16 3-8h4"/>',
    plus: '<path d="M12 5v14M5 12h14"/>', close: '<path d="m6 6 12 12M6 18 18 6"/>',
    edit: '<path d="m15 4 5 5M4 20l5-1L20 8a3 3 0 0 0-4-4L5 15z"/>',
    trash: '<path d="M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7M14 10v7"/>',
    arrow: '<path d="M4 12h16m-6-6 6 6-6 6"/>', refresh: '<path d="M20 7a9 9 0 1 0 1 8M20 3v5h-5"/>',
    shield: '<path d="m12 3 8 3v6c0 4-8 9-8 9s-8-5-8-9V6z"/><path d="m8 12 3 3 5-6"/>',
    logout: '<path d="M9 4H4v16h5M10 12h11m-4-4 4 4-4 4"/>',
    clock: '<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
    leaf: '<path d="M20 3C8 2 2 8 5 15c4 7 15 2 15-12ZM4 21l11-11"/>',
    spark: '<path d="m12 3 2.5 6.5L21 12l-6.5 2.5L12 21l-2.5-6.5L3 12l6.5-2.5z"/>',
    database: '<ellipse cx="12" cy="5" rx="8" ry="3"/><path d="M4 5v14c0 4 16 4 16 0V5M4 12c0 4 16 4 16 0"/>',
    chip: '<rect x="6" y="6" width="12" height="12" rx="3"/><path d="M9 2v4m6-4v4M9 18v4m6-4v4M2 9h4m-4 6h4m12-6h4m-4 6h4"/>',
    graph: '<circle cx="6" cy="6" r="3"/><circle cx="18" cy="8" r="3"/><circle cx="10" cy="19" r="3"/><path d="m9 6 6 1M7 9l2 7m8-5-5 5"/>',
    link: '<path d="m9 15 6-6M8 8l3-3a4 4 0 0 1 6 6l-2 2M9 11l-2 2a4 4 0 0 0 6 6l3-3"/>',
    chevron: '<path d="m9 5 7 7-7 7"/>', back: '<path d="m15 5-7 7 7 7"/>',
    info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v6m0-10v.1"/>',
  };
  const icon = name => `<svg class="icon" viewBox="0 0 24 24" aria-hidden="true">${paths[name] || paths.book}</svg>`;
  const state = { chatBusy: false, authenticated: false, route: 'chat', libraryView: 'memories', epoch: 0, controller: new AbortController(), filter: { q: '', kind: '', status: 'all', sort: 'updated', offset: 0 }, detail: null, library: null, poll: 0, agentPanels: {} };
  const fmt = n => Number(n || 0).toLocaleString('zh-CN');
  const pct = n => `${Math.round(Number(n || 0) * 100)}%`;
  const date = (n, full = false) => n ? new Date(n * 1000).toLocaleString('zh-CN', { month: '2-digit', day: '2-digit', ...(full ? { year: 'numeric', hour: '2-digit', minute: '2-digit' } : {}) }) : '—';
  const ms = n => Number.isFinite(n) ? (n < 1000 ? `${n.toFixed(0)} ms` : `${(n / 1000).toFixed(2)} s`) : '—';
  const kindBadge = kind => `<span class="badge kind-${Object.hasOwn(kinds, kind) ? kind : 'fact'}">${esc(kinds[kind] || kind)}</span>`;
  function memoryState(m) { return m.deleted_at ? ['已删除', 'red'] : m.status === 'superseded' ? ['已替代', ''] : m.status === 'forgotten' ? ['已遗忘', ''] : m.valid_until && m.valid_until <= Date.now() / 1000 ? ['已过期', 'amber'] : ['有效', 'green']; }
  const statusBadge = m => { const [text, color] = memoryState(m); return `<span class="badge ${color}"><i class="dot"></i>${text}</span>`; };
  const signal = n => `<span class="signal"><meter min="0" max="1" value="${Number(n)}" aria-label="${pct(n)}"></meter>${pct(n)}</span>`;
  const entityArray = m => { try { return Array.isArray(m.entities) ? m.entities : JSON.parse(m.entities); } catch { return []; } };
  function toast(text, error = false) { const el = $('#toast'); el.textContent = text; el.className = `show${error ? ' error' : ''}`; clearTimeout(toast.timer); toast.timer = setTimeout(() => { el.className = ''; el.textContent = ''; }, 4200); }
  class ApiFailure extends Error { constructor(status, code) { super(code); this.status = status; this.code = code; } }
  async function api(path, options = {}) {
    const response = await fetch(path, { ...options, cache: 'no-store', credentials: 'same-origin', redirect: 'error', signal: state.controller.signal, headers: { Accept: 'application/json', ...(options.body ? { 'Content-Type': 'application/json' } : {}), ...options.headers } });
    const body = await response.json();
    if (!response.ok) {
      if (response.status === 401 && state.authenticated && $('#page')) { clearLogin(); toast('登录已失效，请重新登录。', true); }
      throw new ApiFailure(response.status, body.error || 'request_failed');
    }
    return body;
  }
  const send = (path, data, method = 'POST') => api(path, { method, body: JSON.stringify(data) });
  function errorText(error) {
    const personaErrors = { evolution_changed: '进化版本已经变化，请刷新后重试。', invalid_evolution_lock: '锁定项已变化，请刷新后重试。', invalid_evolution_evidence: '证据必须来自真实已完成用户对话。', evolution_paused_busy_or_changed: '进化已暂停、正在运行或配置已变化，请刷新后重试。', evolution_version_missing: '找不到该进化版本。', persona_derivation_changed: '人格或画像已发生变化，请刷新后核对最新版本。', persona_generation_running: '画像正在生成，请等待完成。', invalid_persona_derivation: '请检查画像：每项倾向最多 300 字、依据最多 150 字，参考标签最多 3 个。', persona_derivation_budget: '画像超过当前上下文预算，请缩短后保存。', invalid_preview_message: '请输入 1–4000 字的示例消息。', persona_preview_failed: '预览未完成，请检查模型连接后重试。', persona_version_missing: '该画像版本已不在最近五版记录中。' };
    if (personaErrors[error.code]) return personaErrors[error.code];
    const stickerErrors = { invalid_sticker: '名字须为 1–40 字，使用场景最多 120 字。', invalid_sticker_image: '图片未能处理，请检查格式、动图帧数与工具服务；处理后的图片须小于 2 MiB。', sticker_catalog_full: '表情库已达到数量或上下文容量上限，请精简使用场景或删除部分表情。', sticker_storage_full: '表情图片存储已达到 64 MiB 上限。', sticker_not_found: '表情包已被删除，请刷新列表。', tools_disabled: '请先启用工具服务，再上传表情包。' };
    if (stickerErrors[error.code]) return stickerErrors[error.code];
    if (error.code === 'duplicate_memory') return '已有相同内容的有效记忆，请编辑已有记录。';
    if (error.code === 'memory_changed_reload') return '这条记忆已发生变化，请关闭窗口后重新打开，核对最新版本。';
    if (error.code === 'invalid_persona') return '请填写名字（最多 80 字）；性格各项合计最多 6000 字，并须符合当前上下文容量。';
    if (error.code === 'invalid_password') return '新密码须为 10–128 个字符，且不能全部为空格。';
    if (error.code === 'current_password_incorrect') return '当前密码不正确，请重新输入。';
    if (error.code === 'agent_busy') return 'Agent 正在处理请求或后台任务，请稍后重试。';
    if (error.code === 'reset_confirmation_required') return '请输入 RESET 确认重置。';
    if (error.code === 'login_rate_limited') return '尝试过于频繁，请稍等一秒后重试。';
    if (error.status === 401) return '登录凭证不正确或已失效，请重新输入。';
    if (error.status === 400 || error.status === 422) return '请检查输入：内容最多 500 字，实体最多 8 个，评分须在 0–100% 之间。';
    if (error.status === 404) return '没有找到这条记忆，请刷新列表。';
    return '暂时无法完成请求，请检查服务连接后重试。';
  }
  function empty(title, description, action = '') { return `<div class="empty"><div class="empty-icon">${icon('leaf')}</div><h3>${esc(title)}</h3><p>${esc(description)}</p>${action}</div>`; }
  const loading = text => `<div class="loading" role="status"><span class="spinner"></span>${esc(text || '正在读取记忆…')}</div>`;
  const newButton = () => `<button class="btn primary" data-new>${icon('plus')}新增记忆</button>`;
  function login() {
    $('#app').innerHTML = `<main class="login"><section class="login-story"><a class="brand" href="/console/" aria-label="Mneme Console"><img src="/console/icon.svg" alt=""><div class="brand-text"><div class="brand-name">mneme</div><div class="brand-caption">CONSOLE</div></div></a><div class="login-copy"><div class="eyebrow">A SPACE FOR WHAT MATTERS</div><h1 class="serif">每一段记忆，<br>都值得被好好保管。</h1><p>在这里，整理那些值得留下的片段，<br>让每一次回想，都有迹可循。</p></div><div class="login-footer">PRIVATE BY DEFAULT · BUILT WITH CARE</div><div class="login-decoration"></div></section><section class="login-form-wrap"><form id="login-form" class="login-card"><div class="eyebrow">YOUR PRIVATE CONSOLE</div><h2>进入你的记忆空间</h2><p class="subheading">使用 WebUI 密码验证身份。</p><label class="field" for="password">登录密码<input id="password" name="password" type="password" placeholder="输入你的 WebUI 密码" autocomplete="current-password" minlength="10" maxlength="128" required autofocus></label><button class="btn primary" type="submit">解锁控制台${icon('arrow')}</button><div id="login-error" class="error-inline" role="alert"></div><div class="login-note">${icon('shield')}<span>登录状态安全保存在浏览器中，刷新或重新打开页面后可继续使用。主动退出或登录过期后需重新验证。</span></div></form></section></main>`;
    api('/v1/console/login').then(data => {
      if (!data.password_configured && $('#login-error')) $('#login-error').textContent = '尚未设置登录密码。请运行安装脚本生成密码，或配置 MNEME_CONSOLE_PASSWORD 后重启服务。';
    }).catch(() => {});
    $('#login-form').addEventListener('submit', async event => {
      event.preventDefault(); const button = $('button[type=submit]', event.target); button.disabled = true; $('#login-error').textContent = '';
      const credential = $('#password').value; $('#password').value = '';
      try {
        await send('/v1/console/login', { password: credential }); state.authenticated = true;
        shell(); route();
      }
      catch (error) { state.authenticated = false; $('#login-error').textContent = errorText(error); button.disabled = false; $('#password').focus(); }
    });
  }
  async function logout() {
    try {
      const response = await fetch('/v1/console/logout', { method: 'POST', credentials: 'same-origin', cache: 'no-store', keepalive: true });
      if (!response.ok && response.status !== 401) throw new Error('logout_failed');
    } catch { toast('退出未完成，请检查连接后重试。', true); return; }
    clearLogin();
  }
  function clearLogin() {
    for (const url of objectURLs) URL.revokeObjectURL(url); objectURLs.clear();
    state.chatBusy = false; lazyDetails.clear(); state.agentPanels = {};
    state.authenticated = false; state.controller.abort(); state.controller = new AbortController(); state.epoch++; state.detail = null; state.library = null; state.libraryView = 'memories';
    state.filter = { q: '', kind: '', status: 'all', sort: 'updated', offset: 0 }; clearTimeout(state.poll);
    for (const modal of $$('dialog')) { if (modal.open) modal.close(); modal.innerHTML = ''; }
    setNavigation(false); delete document.body.dataset.page;
    $('#toast').textContent = ''; $('#toast').className = ''; document.body.classList.remove('dialog-open'); login();
  }
  function setNavigation(open) {
    document.body.classList.toggle('nav-open', open);
    $('#menu-toggle')?.setAttribute('aria-expanded', String(open));
  }
  function shell() {
    const navigation = key => `<a href="#${key}" data-nav="${key}" aria-label="${pages[key][0]}">${icon({ chat: 'spark', memories: 'book', agent: 'leaf', tools: 'chip', retrieval: 'search', system: 'system', settings: 'edit' }[key])}<span>${pages[key][0]}</span></a>`;
    $('#app').innerHTML = `<button class="menu-toggle icon-btn" id="menu-toggle" aria-label="打开导航" aria-expanded="false" aria-controls="sidebar">${icon('grid')}</button><button class="nav-backdrop" id="nav-backdrop" aria-label="关闭导航" tabindex="-1"></button><aside class="sidebar" id="sidebar" aria-label="主导航"><a class="brand" href="#chat" aria-label="Mneme 对话"><img src="/console/icon.svg" alt=""><div class="brand-text"><div class="brand-name">mneme</div><div class="brand-caption">YOUR COMPANION</div></div></a><nav class="nav">${['chat', 'memories', 'agent'].map(navigation).join('')}<div class="nav-separator"></div>${['tools', 'retrieval', 'system'].map(navigation).join('')}</nav><div class="sidebar-foot"><nav class="nav account-nav">${navigation('settings')}</nav><div class="account-row"><div class="verified"><i class="dot"></i><span>身份已验证</span></div><button id="logout" class="icon-btn" aria-label="退出登录" title="退出登录">${icon('logout')}</button></div></div></aside><div class="workspace"><main class="page" id="main-content"><div id="page" aria-live="polite"></div></main></div>`;
    $('#logout').onclick = logout;
    $('#menu-toggle').onclick = () => setNavigation(!document.body.classList.contains('nav-open'));
    $('#nav-backdrop').onclick = () => setNavigation(false);
    $$('.sidebar [data-nav]').forEach(link => { link.onclick = () => setNavigation(false); });
  }
  async function route() {
    if (!state.authenticated) return;
    for (const url of objectURLs) URL.revokeObjectURL(url); objectURLs.clear();
    clearTimeout(state.poll); const epoch = ++state.epoch;
    const requested = location.hash.slice(1) === 'overview' ? 'memories' : location.hash.slice(1);
    state.route = Object.hasOwn(pages, requested) ? requested : 'chat';
    document.body.dataset.page = state.route;
    $$('[data-nav]').forEach(link => { const active = link.dataset.nav === state.route; link.classList.toggle('active', active); if (active) link.setAttribute('aria-current', 'page'); else link.removeAttribute('aria-current'); });
    document.title = `${pages[state.route][0]} · Mneme Console`;
    $('#page').innerHTML = loading(state.route === 'system' ? '正在探测服务状态…' : '正在读取数据…');
    try { await ({ memories: library, agent: agentPage, retrieval: retrievalPage, system: systemPage, settings: settingsPage, chat: chatPage, tools: toolsPage }[state.route])(epoch); }
    catch (error) { if (epoch === state.epoch && error.name !== 'AbortError') $('#page').innerHTML = `<section class="panel">${empty('暂时无法读取数据', errorText(error), '<button class="btn" data-reload>重新加载</button>')}</section>`; }
  }
  function focusSearch() { if (!state.authenticated) return; if (state.route === 'memories') { if (state.libraryView === 'queue') selectLibraryView('memories'); $('#memory-search')?.focus(); } else { state.libraryView = 'memories'; location.hash = 'memories'; setTimeout(() => $('#memory-search')?.focus(), 150); } }
  function memorySummary(stats) {
    const pending = Number(stats.jobs.pending || 0) + Number(stats.jobs.running || 0);
    const cards = [['记忆总量', stats.total, '所有未删除的记忆', 'book'], ['有效记忆', stats.active, `${fmt(stats.invalid)} 条已失效`, 'leaf'], ['近 7 天新增', stats.recent_7_days, '持续积累的片段', 'spark'], ['后台任务', pending, stats.jobs.dead ? `${fmt(stats.jobs.dead)} 个任务需要重试` : '提取与索引同步', 'clock']];
    return `<section class="metrics" aria-label="记忆统计">${cards.map(([label, n, hint, glyph], i) => `<article class="metric${i === 1 ? ' featured' : ''}"><div class="metric-label">${label}${icon(glyph)}</div><div class="metric-number"${i === 3 ? ' id="queue-metric"' : ''}>${fmt(n)}<small>${i === 3 ? '个' : '条'}</small></div><div class="metric-hint"${i === 3 ? ' id="queue-metric-hint"' : ''}>${esc(hint)}</div></article>`).join('')}</section><section class="charts memory-charts"><article class="panel"><div class="panel-head between"><h2>记忆趋势</h2><small>近 7 天 · UTC</small></div><div class="chart-wrap">${activityChart(stats.activity)}</div></article><article class="panel"><div class="panel-head"><h2>类型分布</h2></div>${distribution(stats.kinds, stats.total)}</article></section>`;
  }
  function activityChart(activity) {
    const max = Math.max(1, ...activity.map(a => a.count)); const grid = [0, 1, 2, 3].map(i => `<line class="chart-grid" x1="25" x2="575" y1="${25 + i * 36}" y2="${25 + i * 36}"/>`).join('');
    return `<svg class="activity-chart" viewBox="0 0 600 180" role="img" aria-label="最近七个自然日新增记忆柱状图">${grid}${activity.map((a, i) => { const h = a.count / max * 105; const x = 47 + i * 78; return `<g><rect class="bar${i === 6 ? ' last' : ''}" x="${x}" y="${135 - Math.max(h, 2)}" width="31" height="${Math.max(h, 2)}" rx="4"><title>${date(a.day)}：${a.count} 条</title></rect><text x="${x + 15.5}" y="${121 - h}" text-anchor="middle">${a.count || ''}</text><text x="${x + 15.5}" y="160" text-anchor="middle">${date(a.day)}</text></g>`; }).join('')}</svg>`;
  }
  function distribution(counts, total) {
    let offset = 0; const circles = Object.keys(kinds).map(kind => { const pct = total ? counts[kind] / total * 100 : 0; const start = offset; offset += pct; return `<circle cx="80" cy="80" r="60" fill="none" stroke="${colors[kind]}" stroke-width="14" pathLength="100" stroke-dasharray="${Math.max(0, pct - (pct ? 1.4 : 0))} ${100 - Math.max(0, pct - (pct ? 1.4 : 0))}" stroke-dashoffset="${-start}"/>`; }).join('');
    return `<div class="distribution"><div class="donut"><svg viewBox="0 0 160 160" aria-hidden="true"><circle cx="80" cy="80" r="60" fill="none" stroke="#f0f3eb" stroke-width="14"/>${circles}</svg><div class="donut-center"><strong>${fmt(total)}</strong><small>条记忆</small></div></div><div class="legend">${Object.keys(kinds).map(kind => `<div class="legend-row ${kind}"><i class="dot"></i>${kinds[kind]}<strong>${fmt(counts[kind])}</strong></div>`).join('')}</div></div>`;
  }
  async function library(epoch) {
    const stats = await api('/v1/console/stats'); if (epoch !== state.epoch) return;
    const f = state.filter; const queue = state.libraryView === 'queue';
    $('#page').innerHTML = memorySummary(stats) + `<div class="library-toolbar"><div class="tabs" role="group" aria-label="记忆状态">${[['all', '全部记忆'], ['active', '有效'], ['invalid', '已失效'], ['deleted', '已删除']].map(([key, label]) => `<button class="tab${!queue && f.status === key ? ' active' : ''}" data-status="${key}" aria-pressed="${!queue && f.status === key}">${label}</button>`).join('')}<button class="tab${queue ? ' active' : ''}" id="queue-tab" data-queue-view aria-pressed="${queue}">后台队列<span class="tab-count" id="queue-tab-count">${fmt(Number(stats.jobs.pending) + Number(stats.jobs.running) + Number(stats.jobs.dead))}</span></button></div>${newButton()}</div><section class="panel" id="memory-browser"${queue ? ' hidden' : ''}><form class="filters" id="filter-form"><div class="search-field">${icon('search')}<label class="sr-only" for="memory-search">搜索记忆</label><input id="memory-search" type="search" value="${esc(f.q)}" placeholder="搜索内容、实体或属性键…" maxlength="500" autocomplete="off"></div><label class="sr-only" for="kind-filter">筛选记忆类型</label><select id="kind-filter" class="filter-select"><option value="">所有类型</option>${Object.entries(kinds).map(([key, name]) => `<option value="${key}"${f.kind === key ? ' selected' : ''}>${name}</option>`).join('')}</select><label class="sr-only" for="sort-filter">排序方式</label><select id="sort-filter" class="filter-select">${[['updated', '最近更新'], ['created', '最近创建'], ['oldest', '最早创建'], ['importance', '重要性优先'], ['confidence', '可信度优先']].map(([key, name]) => `<option value="${key}"${f.sort === key ? ' selected' : ''}>${name}</option>`).join('')}</select></form><div id="library-results">${loading()}</div></section>${queueMarkup(stats, queue)}`;
    $('#filter-form').onsubmit = event => { event.preventDefault(); clearTimeout(library.debounce); state.filter.q = $('#memory-search').value; state.filter.offset = 0; loadLibrary(epoch); };
    $('#memory-search').oninput = () => { clearTimeout(library.debounce); library.debounce = setTimeout(() => { if (epoch !== state.epoch) return; state.filter.q = $('#memory-search').value; state.filter.offset = 0; loadLibrary(epoch); }, 280); };
    $('#kind-filter').onchange = event => { state.filter.kind = event.target.value; state.filter.offset = 0; loadLibrary(epoch); };
    $('#sort-filter').onchange = event => { state.filter.sort = event.target.value; state.filter.offset = 0; loadLibrary(epoch); };
    if (queue) await refreshQueue(epoch); else await loadLibrary(epoch);
  }
  function queueMarkup(stats, visible) {
    return `<section class="panel job-card memory-queue" id="memory-queue"${visible ? '' : ' hidden'} aria-label="后台处理队列"><div class="between"><div><h2>后台处理队列</h2><p class="subheading">记忆提取和索引同步持续在后台进行</p></div><span class="badge green">自动更新</span></div><div class="queue-counts">${[['pending', '待处理'], ['running', '运行中'], ['dead', '失败']].map(([key, label]) => `<div><span>${label}</span><strong id="queue-${key}">${fmt(stats.jobs[key])}</strong></div>`).join('')}</div><div class="queue-footer"><small id="queue-updated">更新于 ${date(stats.as_of, true)}</small><div class="flex"><button class="btn" data-maintenance="reindex">重建索引</button><button class="btn" data-maintenance="jobs/retry"${stats.jobs.dead ? '' : ' disabled'}>重试失败任务</button></div></div><div class="error-inline" id="queue-error" role="status"></div></section>`;
  }
  function selectLibraryView(view) {
    clearTimeout(state.poll); state.libraryView = view;
    const queue = view === 'queue';
    $('#memory-browser').hidden = queue; $('#memory-queue').hidden = !queue;
    $$('[data-status]').forEach(tab => { const active = !queue && tab.dataset.status === state.filter.status; tab.classList.toggle('active', active); tab.setAttribute('aria-pressed', String(active)); });
    $('#queue-tab').classList.toggle('active', queue); $('#queue-tab').setAttribute('aria-pressed', String(queue));
    if (queue) refreshQueue(); else loadLibrary();
  }
  async function refreshQueue(epoch = state.epoch) {
    if (epoch !== state.epoch || state.libraryView !== 'queue') return;
    clearTimeout(state.poll);
    try {
      const stats = await api('/v1/console/stats');
      if (epoch !== state.epoch || state.libraryView !== 'queue') return;
      for (const key of ['pending', 'running', 'dead']) $(`#queue-${key}`).textContent = fmt(stats.jobs[key]);
      const pending = Number(stats.jobs.pending) + Number(stats.jobs.running);
      $('#queue-tab-count').textContent = fmt(pending + Number(stats.jobs.dead));
      $('#queue-metric').innerHTML = `${fmt(pending)}<small>个</small>`;
      $('#queue-metric-hint').textContent = stats.jobs.dead ? `${fmt(stats.jobs.dead)} 个任务需要重试` : '提取与索引同步';
      const retry = $('#memory-queue [data-maintenance="jobs/retry"]');
      if (!retry.dataset.busy) retry.disabled = !stats.jobs.dead;
      $('#queue-updated').textContent = `更新于 ${date(stats.as_of, true)}`;
      $('#queue-error').textContent = '';
    } catch (error) {
      if (epoch !== state.epoch || state.libraryView !== 'queue' || error.name === 'AbortError') return;
      $('#queue-error').textContent = '队列状态暂时无法更新，稍后会自动重试。';
    }
    if (epoch === state.epoch && state.libraryView === 'queue') state.poll = setTimeout(() => refreshQueue(epoch), 5000);
  }
  async function loadLibrary(epoch = state.epoch) {
    const sequence = library.sequence = (library.sequence || 0) + 1; const element = $('#library-results'); if (!element) return;
    element.setAttribute('aria-busy', 'true');
    try {
      const query = new URLSearchParams({ ...state.filter, limit: 15 }); const data = await api(`/v1/memories?${query}`);
      if (epoch !== state.epoch || sequence !== library.sequence) return;
      state.library = data; element.removeAttribute('aria-busy');
      element.innerHTML = data.memories.length ? `<div class="table-scroll"><table class="library-table"><thead><tr><th>记忆内容</th><th>类型</th><th>状态</th><th>重要性</th><th>可信度</th><th>更新时间</th><th><span class="sr-only">操作</span></th></tr></thead><tbody>${data.memories.map(m => `<tr><td class="memory-cell"><button class="text-btn memory-title" data-detail="${esc(m.id)}">${esc(m.content)}</button><div class="memory-meta">${esc(entityArray(m).slice(0, 3).map(e => `# ${e}`).join('   ')) || (m.origin === 'manual' ? '手动记录' : '来自对话')}${m.indexed_revision < m.revision ? ' · 索引更新中' : ''}</div></td><td class="cell-kind">${kindBadge(m.kind)}</td><td class="cell-state">${statusBadge(m)}</td><td class="cell-score" data-label="重要性">${signal(m.importance)}</td><td class="cell-score" data-label="可信度">${signal(m.confidence)}</td><td class="cell-time table-time">${date(m.updated_at, true)}</td><td class="cell-actions"><div class="actions"><button class="icon-btn table-action" data-edit="${esc(m.id)}" aria-label="编辑记忆"${m.deleted_at || m.status !== 'active' ? ' disabled' : ''}>${icon('edit')}</button><button class="icon-btn table-action danger" data-delete="${esc(m.id)}" aria-label="删除记忆"${m.deleted_at ? ' disabled' : ''}>${icon('trash')}</button></div></td></tr>`).join('')}</tbody></table></div>${pagination(data)}` : empty(state.filter.q || state.filter.kind || state.filter.status !== 'all' ? '没有找到匹配的记忆' : '这里还没有记忆', state.filter.status === 'deleted' ? '已删除的记忆不会参与召回。' : '试试其他关键词或筛选条件，或者手动新增一条记忆。', state.filter.status === 'all' && !state.filter.q ? newButton() : '') + pagination(data);
      if (state.libraryView !== 'queue' && data.memories.some(m => m.indexed_revision < m.revision)) { clearTimeout(state.poll); state.poll = setTimeout(() => { if (epoch === state.epoch && state.libraryView !== 'queue' && !$('#editor').open && !$('#confirm').open) loadLibrary(epoch); }, 5000); }
    } catch (error) { if (epoch === state.epoch && error.name !== 'AbortError') { element.removeAttribute('aria-busy'); element.innerHTML = empty('无法读取记忆', errorText(error), '<button class="btn" data-reload>重试</button>'); } }
  }
  function pagination(data) { return `<div class="pagination"><span>共 ${fmt(data.total)} 条${data.total ? ` · ${data.offset + 1}–${Math.min(data.offset + data.limit, data.total)}` : ''}</span><div class="flex"><button class="btn small" data-page="${Math.max(0, data.offset - data.limit)}"${data.offset === 0 ? ' disabled' : ''}>${icon('back')}上一页</button><button class="btn small" data-page="${data.offset + data.limit}"${data.offset + data.limit >= data.total ? ' disabled' : ''}>下一页${icon('chevron')}</button></div></div>`; }
  function openDialog(id, html) { const dialog = $(`#${id}`); dialog.innerHTML = html; if (!dialog.open) dialog.showModal(); document.body.classList.add('dialog-open'); return dialog; }
  function closeDialog(id) { const dialog = $(`#${id}`); if (dialog.open) dialog.close(); dialog.innerHTML = ''; if (!$$('dialog[open]').length) document.body.classList.remove('dialog-open'); }
  async function detail(id) {
    const epoch = state.epoch; const sequence = detail.sequence = (detail.sequence || 0) + 1; openDialog('detail', `<div class="dialog-head"><h2>记忆详情</h2><button class="icon-btn" data-close="detail" aria-label="关闭详情">${icon('close')}</button></div>${loading()}`);
    try { const data = await api(`/v1/memories/${encodeURIComponent(id)}`); if (epoch !== state.epoch || sequence !== detail.sequence || !$('#detail').open) return; state.detail = data; renderDetail(data); }
    catch (error) { if (error.name !== 'AbortError' && epoch === state.epoch) { closeDialog('detail'); toast(errorText(error), true); } }
  }
  function renderDetail(data) {
    const m = data.memory; const entities = entityArray(m); const editable = !m.deleted_at && m.status === 'active';
    openDialog('detail', `<header class="dialog-head"><div><div class="eyebrow">MEMORY DETAIL</div><h2>每一段记忆，都有来处</h2></div><button class="icon-btn" data-close="detail" aria-label="关闭详情">${icon('close')}</button></header><div class="dialog-body"><div class="between"><span class="flex">${kindBadge(m.kind)}${statusBadge(m)}</span><small>${m.origin === 'manual' ? '手动整理' : '自动提取'}</small></div><div class="detail-content serif prewrap">${esc(m.content)}</div><dl class="detail-facts"><div><dt>重要性</dt><dd>${signal(m.importance)}</dd></div><div><dt>可信度</dt><dd>${signal(m.confidence)}</dd></div><div><dt>创建时间</dt><dd>${date(m.created_at, true)}</dd></div><div><dt>更新时间</dt><dd>${date(m.updated_at, true)}</dd></div><div><dt>有效期至</dt><dd>${m.valid_until ? date(m.valid_until, true) : '未设置期限'}</dd></div><div><dt>检索索引</dt><dd>${m.indexed_revision >= m.revision ? '已同步' : '等待后台更新'}</dd></div></dl><section class="detail-section"><h3>${icon('graph')}关联实体</h3><div class="entity-list">${entities.length ? entities.map(e => `<span class="tag"># ${esc(e)}</span>`).join('') : '<small>暂未关联实体</small>'}</div></section><section class="detail-section"><h3>${icon('link')}来源消息<small>${data.sources.length} 条</small></h3>${m.origin === 'manual' ? '<div class="notice">手动整理的内容以你的修改为准；已有来源仅用于追溯原始对话。</div>' : ''}${data.sources.length ? data.sources.map(s => `<details class="source"><summary>来自对话 · ${date(s.observed_at, true)}</summary><blockquote class="prewrap">${esc(s.quote)}</blockquote>${s.content ? `<p class="prewrap">${esc(s.content)}</p>` : ''}<div class="technical">会话：${esc(s.session_id)}<br>消息：${esc(s.message_id)}</div></details>`).join('') : '<small>这条记忆由手动创建，没有对话来源。</small>'}</section><section class="detail-section"><h3>${icon('clock')}历史版本<small>最近 ${data.versions.length} 次修订</small></h3>${data.versions.map((v, i) => `<details class="version"${i === 0 ? ' open' : ''}><summary>${i === 0 ? '当前记录' : '历史记录'} · 修订 ${v.revision} · ${date(v.recorded_at, true)}</summary><p class="prewrap">${esc(v.snapshot.content)}</p><div class="flex">${kindBadge(v.snapshot.kind)}${statusBadge(v.snapshot)}<small>${v.snapshot.origin === 'manual' ? '手动整理' : '系统整理'} · 重要性 ${pct(v.snapshot.importance)}</small></div></details>`).join('') || '<small>暂无历史记录</small>'}</section><details class="technical"><summary>标识与属性键</summary><p>ID：${esc(m.id)}</p><p>属性：${esc(m.slot)}</p><p>修订：${m.revision} · 已同步修订：${m.indexed_revision}</p></details></div><footer class="dialog-footer">${!m.deleted_at ? `<button class="btn ghost footer-start" data-delete="${esc(m.id)}">${icon('trash')}删除</button>` : '<small class="footer-start">已停止召回</small>'}<button class="btn" data-close="detail">关闭</button>${editable ? `<button class="btn primary" data-edit="${esc(m.id)}">${icon('edit')}编辑记忆</button>` : ''}</footer>`);
  }
  function datetimeLocal(timestamp) { if (!timestamp) return ''; const d = new Date(timestamp * 1000); return new Date(d.getTime() - d.getTimezoneOffset() * 60000).toISOString().slice(0, 16); }
  async function edit(id) {
    const epoch = state.epoch; let m = null;
    if (id) { try { m = (await api(`/v1/memories/${encodeURIComponent(id)}`)).memory; } catch (error) { if (error.name !== 'AbortError') toast(errorText(error), true); return; } }
    if (epoch !== state.epoch) return;
    if (m && (m.deleted_at || m.status !== 'active')) { toast('这条记忆已失效，不能直接编辑。', true); return; }
    const dialog = openDialog('editor', `<form id="memory-form"><header class="dialog-head"><div><div class="eyebrow">${m ? 'EDIT MEMORY' : 'NEW MEMORY'}</div><h2>${m ? '重新整理一段记忆' : '留下一段值得记住的事'}</h2><p>保存后自动更新检索索引。</p></div><button type="button" class="icon-btn" data-close="editor" aria-label="关闭编辑">${icon('close')}</button></header><div class="dialog-body form-grid"><label class="field full">记忆内容<textarea name="content" id="memory-content" maxlength="500" required placeholder="例如：用户喜欢不加糖的拿铁。">${esc(m?.content || '')}</textarea><span class="hint between"><span>用一句清晰的陈述，记录一件具体的事。</span><span id="char-count">${[...(m?.content || '')].length}/500</span></span></label><label class="field">记忆类型<select name="kind">${Object.entries(kinds).map(([key, name]) => `<option value="${key}"${m?.kind === key ? ' selected' : ''}>${name}</option>`).join('')}</select></label><label class="field">有效期至<input type="datetime-local" name="valid_until" value="${datetimeLocal(m?.valid_until)}"><span class="hint">留空表示不设置期限 · 当前本地时区</span></label><label class="field"><span class="range-label">重要性<output id="importance-output">${pct(m?.importance ?? 0.7)}</output></span><input type="range" name="importance" min="0" max="100" value="${Math.round((m?.importance ?? 0.7) * 100)}"><span class="hint">对未来交流的价值</span></label><label class="field"><span class="range-label">可信度<output id="confidence-output">${pct(m?.confidence ?? 1)}</output></span><input type="range" name="confidence" min="0" max="100" value="${Math.round((m?.confidence ?? 1) * 100)}"><span class="hint">对这条信息的确信程度</span></label><label class="field full">关联实体<input name="entities" value="${esc(m ? entityArray(m).join('，') : '')}" placeholder="用逗号分隔，如：成都，咖啡" maxlength="488"><span class="hint">最多 8 个，每个不超过 60 字</span></label><div class="error-inline full" id="form-error" role="alert"></div></div><footer class="dialog-footer"><button type="button" class="btn" data-close="editor">取消</button><button type="submit" class="btn primary" id="save-memory">${m ? '保存修改' : '创建记忆'}${icon('arrow')}</button></footer></form>`);
    $('#memory-content').oninput = event => { $('#char-count').textContent = `${[...event.target.value].length}/500`; };
    for (const name of ['importance', 'confidence']) $(`[name=${name}]`, dialog).oninput = event => { $(`#${name}-output`).textContent = `${event.target.value}%`; };
    $('#memory-form').onsubmit = async event => {
      event.preventDefault(); const form = new FormData(event.target); const button = $('#save-memory'); const errorEl = $('#form-error'); button.disabled = true; errorEl.textContent = '';
      const value = { content: form.get('content').trim(), kind: form.get('kind'), importance: Number(form.get('importance')) / 100, confidence: Number(form.get('confidence')) / 100, entities: [...new Set(form.get('entities').split(/[,，、\n]/).map(e => e.trim()).filter(Boolean))], valid_until: form.get('valid_until') ? Math.floor(new Date(form.get('valid_until')).getTime() / 1000) : null, ...(m ? { expected_revision: m.revision } : {}) };
      if (!value.content || [...value.content].length > 500 || value.entities.length > 8 || value.entities.some(e => [...e].length > 60)) { errorEl.textContent = '请检查内容长度和实体数量。'; button.disabled = false; return; }
      try { const data = await send(m ? `/v1/memories/${encodeURIComponent(m.id)}` : '/v1/memories', value, m ? 'PUT' : 'POST'); closeDialog('editor'); if ($('#detail').open && m) await detail(data.memory.id); toast(m ? '记忆已更新，索引将在后台同步。' : '记忆已创建，索引将在后台同步。'); route(); }
      catch (error) { if (error.name !== 'AbortError' && state.authenticated) { errorEl.textContent = errorText(error); button.disabled = false; } }
    };
  }
  async function remove(id) {
    const epoch = state.epoch; let m;
    try { m = (await api(`/v1/memories/${encodeURIComponent(id)}`)).memory; } catch (error) { if (error.name !== 'AbortError') toast(errorText(error), true); return; }
    if (epoch !== state.epoch) return;
    const dialog = openDialog('confirm', `<div class="dialog-head"><div><div class="eyebrow">DELETE MEMORY</div><h2>删除这段记忆？</h2></div><button class="icon-btn" data-close="confirm" aria-label="取消删除">${icon('close')}</button></div><div class="dialog-body"><div class="delete-preview prewrap">${esc(m.content.slice(0, 180))}${m.content.length > 180 ? '…' : ''}</div><p class="subheading">删除后不再参与召回。原始来源与修订记录会保留，便于追溯。</p><div id="delete-error" class="error-inline spaced" role="alert"></div></div><div class="dialog-footer"><button class="btn" data-close="confirm">保留记忆</button><button class="btn danger" id="confirm-delete">确认删除</button></div>`);
    $('#confirm-delete', dialog).onclick = async event => { const button = event.currentTarget; button.disabled = true; try { await api(`/v1/memories/${encodeURIComponent(id)}?expected_revision=${m.revision}`, { method: 'DELETE' }); closeDialog('confirm'); closeDialog('detail'); state.detail = null; toast('记忆已删除，旧任务不会恢复它。'); route(); } catch (error) { if (error.name !== 'AbortError' && state.authenticated) { $('#delete-error').textContent = errorText(error); button.disabled = false; } } };
  }
  async function retrievalPage() {
    $('#page').innerHTML = `<section class="panel"><form class="lab-query" id="retrieval-form"><label for="retrieval-query" class="sr-only">检索问题</label><textarea id="retrieval-query" maxlength="4000" placeholder="输入一个真实的问题，看看 Mneme 会想起什么…" required></textarea><div class="lab-bottom"><div class="examples"><button type="button" class="example" data-example="我平时喜欢喝什么咖啡？">咖啡偏好</button><button type="button" class="example" data-example="我现在住在哪座城市？">目前住址</button></div><button class="btn primary" type="submit" id="run-retrieval">运行检索${icon('arrow')}</button></div></form></section><div id="retrieval-results">${empty('从一个问题，开始回想', '这里会展示真实的召回候选、精排分数、筛除原因与最终结果。')}</div>`;
    $('#retrieval-form').onsubmit = async event => { event.preventDefault(); const query = $('#retrieval-query').value.trim(); if (!query) return; const epoch = state.epoch; const button = $('#run-retrieval'); button.disabled = true; $('#retrieval-results').innerHTML = loading('正在检索与精排…'); try { const data = await send('/v1/console/retrieval', { query }); if (epoch !== state.epoch) return; renderRetrieval(data); } catch (error) { if (epoch === state.epoch && error.name !== 'AbortError') $('#retrieval-results').innerHTML = empty('这次回想没有完成', errorText(error)); } finally { if (epoch === state.epoch) button.disabled = false; } };
  }
  function renderRetrieval(data) {
    const trace = data.trace; const labels = { vector: ['向量召回', 'Neo4j HNSW · 索引分数'], keyword: ['关键词召回', '中文 CJK 全文 · Lucene 分数'], graph_entity: ['实体匹配', '查询中直接提及的实体'], graph_neighbor: ['图谱扩展', '第一层共享实体邻居'], graph_multihop: ['多跳关系', '第二层共享实体扩展'], sqlite_vector: ['近期向量补充', 'SQLite · cosine 相似度'], sqlite_keyword: ['近期关键词补充', 'SQLite · 中文双字重叠'] };
    const reason = { inactive: '已失效', changed_during_search: '检索中发生修改', below_rerank_threshold: '低于精排阈值', context_limit: '超出条数或 Token 预算', below_relative_rerank_threshold: '低于分面相对精排阈值', below_vector_and_rescue_threshold: '语义与补救证据不足', support_changed_during_search: '桥接事实发生修改', supporting_path: '关系链的桥接事实' };
    $('#retrieval-results').innerHTML = `<section class="panel trace-summary"><strong>${data.degraded ? '部分通道不可用，已使用可用结果' : '本次检索已完成'}</strong><span class="badge ${data.degraded ? 'amber' : 'green'}"><i class="dot"></i>${data.degraded ? '降级检索' : '完整检索'}</span><span class="badge">总耗时 ${ms(trace.total_ms)}</span><span class="badge">编码 ${ms(trace.embedding_ms)}</span><span class="badge">精排 ${ms(trace.rerank_ms)}</span></section><section class="panel spaced"><div class="panel-head between"><div><h2>最终想起的记忆</h2><p class="subheading">精排、时效与上下文预算共同决定</p></div><span class="badge green">${data.results.length} 条</span></div>${data.results.length ? `<div class="recall-list">${data.results.map((r, i) => `<article class="recall-row"><span class="hit-rank">${i + 1}</span><div class="recall-content"><button class="text-btn prewrap" data-detail="${esc(r.memory.id)}">${esc(r.memory.content)}</button><small>${r.channels.map(c => esc(labels[c]?.[0] || c)).join(' · ')}</small></div><span class="hit-score">${r.score.toFixed(4)}</span></article>`).join('')}</div>` : empty('没有足够相关的记忆', '可以调整问题，或先检查记忆是否已生效、索引是否已同步。')}</section><div class="spaced between"><div><div class="eyebrow">RECALL CHANNELS</div><h2>候选从哪里来</h2></div><small>不同通道的原始分数不可直接比较</small></div><section class="channel-grid spaced">${trace.channels.map(channel => `<article class="panel"><header class="panel-head between"><div><h2>${esc(labels[channel.name]?.[0] || channel.name)}</h2><p class="subheading">${esc(labels[channel.name]?.[1] || '')}</p></div><span class="badge ${channel.state === 'healthy' ? '' : 'amber'}">${channel.state === 'healthy' ? ms(channel.duration_ms) : '不可用'}</span></header><div class="channel-hits">${channel.hits.length ? channel.hits.map(hit => `<div class="hit"><span class="hit-rank">${hit.rank}</span><div class="hit-body">${hit.content ? `<button class="text-btn" data-detail="${esc(hit.id)}">${esc(hit.content)}</button>` : '<span class="muted">内容已删除或不可用</span>'}${hit.excluded ? '<span class="badge">不参与召回</span>' : ''}</div><span class="hit-score">${hit.score === null ? '—' : hit.score.toFixed(4)}</span></div>`).join('') : '<div class="empty">该通道没有候选</div>'}</div></article>`).join('')}</section><section class="panel spaced"><div class="panel-head"><h2>Reranker 与最终排序</h2><p class="subheading">${trace.reranker_state === 'healthy' ? `精排阈值 ${data.rerank_min_score} · 最终得分包含相关性、融合排名和时间强度` : trace.reranker_state === 'skipped' ? '没有有效候选，未调用精排' : '精排不可用，按 RRF 融合排名回退'}</p></div><div class="table-scroll"><table><thead><tr><th>候选记忆</th><th>精排顺序</th><th>RRF</th><th>Reranker</th><th>最终得分</th><th>结果</th></tr></thead><tbody>${trace.candidates.map(c => `<tr><td class="memory-cell">${esc(c.content || '内容已删除或不可用')}</td><td>${c.rerank_rank ?? '—'}</td><td>${c.rrf_score.toFixed(4)}</td><td>${c.rerank_score === null ? '—' : c.rerank_score.toFixed(4)}</td><td>${c.final_score === null ? '—' : c.final_score.toFixed(4)}</td><td><span class="badge${c.selected ? ' green' : ''}">${c.selected ? '已召回' : esc(reason[c.reason] || '未选中')}</span></td></tr>`).join('') || '<tr><td colspan="6" class="muted">没有有效候选</td></tr>'}</tbody></table></div></section>`;
  }
  async function systemPage(epoch) {
    const data = await api('/v1/console/system'); if (epoch !== state.epoch) return;
    const labels = { llm: ['语言模型', 'spark', '外部兼容 API'], neo4j: ['Neo4j', 'graph', '知识图谱与混合检索'], embedding: ['Embedding', 'chip', '本地 CPU 语义编码'], reranker: ['Reranker', 'system', '本地 CPU 相关性精排'] };
    const services = Object.entries(data.services).map(([key, s]) => { const [label, glyph, desc] = labels[key]; const metric = s.metrics; return `<article class="panel service"><div class="between"><div class="flex"><div class="service-icon">${icon(glyph)}</div><div><div class="service-title">${label}</div><div class="service-model">${desc}</div></div></div><span class="badge ${s.state === 'healthy' ? 'green' : s.state === 'unknown' ? 'amber' : 'red'}"><i class="dot"></i>${s.state === 'healthy' ? '可用' : s.state === 'unknown' ? '待确认' : '不可用'}</span></div><p class="service-model spaced">${esc(s.model || 'Neo4j · Cypher HTTP')}</p><div class="service-metrics"><div><small>探测耗时</small><strong>${s.latency_ms.toFixed(0)}<span>ms</span></strong></div><div><small>平均调用耗时</small><strong>${metric.requests ? ms(metric.average_ms) : '—'}</strong></div><div><small>调用 / 失败</small><strong>${fmt(metric.requests)}<span>/ ${fmt(metric.failures)}</span></strong></div></div></article>`; }).join('');
    $('#page').innerHTML = `<div class="notice">${icon('info')}<span>LLM 通过模型目录接口探测，不执行生成；接口不受支持时显示“待确认”。CPU 模型状态表示服务已加载。性能统计来自当前进程已完成的真实调用。</span></div><section class="service-grid spaced">${services}</section><section class="panel spaced"><div class="system-facts"><div><small>连续运行</small><strong>${uptime(data.uptime_seconds)}</strong></div><div><small>SQLite 数据文件</small><strong>${bytes(data.database_bytes)}</strong></div><div><small>向量维度</small><strong>${data.dimensions}</strong></div><div><small>服务版本</small><strong>v${esc(data.version)}</strong></div></div></section>`;

  }
  const toolLabels = { search: '搜索网页', navigate: '打开网页', read: '阅读网页', click: '点击', type: '输入关键词', scroll: '滚动', screenshot: '网页截图', back: '返回', wait: '等待渲染', close: '关闭浏览器', view_image: '查看图片' };
  const objectURLs = new Set();
  const safeLink = input => { try { const u = new URL(input); return ['http:', 'https:'].includes(u.protocol) && !u.username && !u.password ? u.href : ''; } catch { return ''; } };
  function sources(result) {
    const entries = result?.results?.length ? result.results : result?.url ? [{ title: result.title || result.url, url: result.url }] : [];
    return entries.slice(0, 8).map(source => { const url = safeLink(source.url); return url ? `<a class="source-link" href="${esc(url)}" target="_blank" rel="noreferrer noopener">${icon('link')}${esc(source.title || url)}</a>` : ''; }).join('');
  }
  async function showImage(asset, container) {
    const epoch = state.epoch;
    const response = await fetch(`/v1/assets/${encodeURIComponent(asset)}`, { credentials: 'same-origin', cache: 'no-store', redirect: 'error', signal: state.controller.signal, headers: {} });
    if (!response.ok) { if (response.status === 401) clearLogin(); throw new ApiFailure(response.status, 'image_not_found'); }
    const blob = await response.blob(); if (epoch !== state.epoch) return;
    const url = URL.createObjectURL(blob); objectURLs.add(url);
    const img = document.createElement('img'); img.src = url; img.alt = '会话图片'; img.className = 'chat-image'; container.append(img);
  }
  function toolMarkup(call) {
    return `<article class="tool-entry"><div class="between"><strong>${esc(toolLabels[call.action] || call.name)}</strong><span class="badge ${call.status === 'completed' ? 'green' : call.status === 'failed' ? 'red' : 'amber'}">${call.status === 'completed' ? '已完成' : call.status === 'failed' ? '失败' : '执行中'}${call.elapsed_ms != null ? ` · ${ms(call.elapsed_ms)}` : ''}</span></div>${call.error ? `<p class="error-inline">${esc(call.error)}</p>` : ''}<div class="source-list">${sources(call.result)}</div>${call.result?.asset_id ? `<button class="btn small" data-image="${esc(call.result.asset_id)}">查看图片</button>` : ''}</article>`;
  }
  const contextUsage = context => {
    const percent = Math.round((context.context_used_tokens || 0) / context.context_tokens * 100);
    return `上下文 ≈ ${fmt(context.context_used_tokens)} / ${fmt(context.context_tokens)} tokens · ${percent}%`;
  };
  const tokenValue = (value, estimated = false) => value == null || !Number.isFinite(value) || value < 0 ? '未提供' : `${estimated || !Number.isSafeInteger(value) ? '≈ ' : ''}${fmt(value)}`;
  const tokenLabels = { input_tokens: '输入', output_tokens: '输出（含推理）', reasoning_tokens: '其中推理', total_tokens: '总消耗', cached_input_tokens: '其中缓存命中', cache_creation_input_tokens: '其中缓存写入' };
  const contextLabels = { system: '系统提示词', history: '历史消息', summary: '压缩摘要', memory: '长期记忆', current: '本轮消息', tools: '工具与图片', overhead: '消息封装' };
  const tokenMetric = (usage, key) => {
    const cumulative = !!usage?.reported;
    const reported = cumulative ? (usage.reported[key] || 0) : Number(usage?.[key] != null);
    const estimates = cumulative ? (usage.estimated?.[key] || 0) : Number(usage?.[key] == null && usage?.estimated?.[key] != null);
    const value = cumulative ? (usage.effective?.[key] ?? usage[key]) : (usage?.[key] ?? usage?.estimated?.[key]);
    const missing = cumulative ? usage.requests - reported - estimates : Number(value == null);
    const lower = ['output_tokens', 'total_tokens'].includes(key) && (cumulative ? usage.output_lower_bound_requests > 0 : estimates && usage.estimated.output_is_lower_bound);
    const source = cumulative ? `${reported} 实测 / ${estimates} 估算${missing ? ` / ${missing} 未知` : ''}` : estimates ? '估算' : reported ? '实测' : '';
    return { cumulative, reported, estimates, value, missing, lower, source };
  };
  const tokenRows = usage => Object.entries(tokenLabels).map(([key, label]) => {
    const { cumulative, reported, estimates, value, lower, source } = tokenMetric(usage, key);
    return `<div><dt>${label}</dt><dd>${tokenValue(reported || estimates || (cumulative && usage.requests === 0) ? value : null, estimates)} <small>${source}${cumulative && estimates && usage[key] != null ? ` · 实测合计 ${tokenValue(usage[key])}` : ''}${lower ? '，含已观测下限' : ''}</small></dd></div>`;
  }).join('');
  const distributionMarkup = distribution => `<dl class="token-distribution">${Object.entries(contextLabels).map(([key, label]) => `<div><dt>${label}</dt><dd>≈ ${fmt(distribution?.[key] || 0)}</dd></div>`).join('')}</dl>`;
  function contextDetails(context) {
    const last = context.last_request;
    const totals = context.session_usage;
    const purposeLabels = { chat: '对话与工具循环', compression: '历史压缩', memory: '长期记忆提取', embedding: 'Embedding', reranker: 'Reranker', sticker: '表情包理解', persona_compile: '人格画像生成', persona_preview: '人格预览', persona_reflect: '自主反思', persona_validate: '进化验证' };
    return `<p>下一轮上下文估算 · ${fmt(context.context_used_tokens)} tokens</p>${distributionMarkup(context.distribution)}<p class="hint">按 ${esc(context.tokenizer || '本地分词器')} 估算，记忆沿用最近召回的大小；下一条消息及其召回结果尚未确定。回复预留 ${fmt(context.reply_reserve_tokens)} tokens${context.tool_reserve_tokens ? `，工具运行预留 ${fmt(context.tool_reserve_tokens)}` : ''}。</p>${last ? `<p>最近模型请求 · 输入 ${tokenValue(last.usage.input_tokens)} tokens（API）</p><details class="request-context"><summary>该请求的输入分布（估算）</summary>${distributionMarkup(last.context)}</details><dl class="token-distribution">${tokenRows(last.usage)}</dl>` : ''}<p>会话累计 · ${fmt(totals?.requests || 0)} 次模型请求</p>${totals?.incomplete_requests ? `<p class="hint">${fmt(totals.incomplete_requests)} 次请求未正常结束，用量以已收到的 API 统计为准。</p>` : ''}<dl class="token-distribution">${tokenRows(totals)}</dl>${Object.entries(totals?.by_purpose || {}).map(([purpose, usage]) => `<p class="hint">${purposeLabels[purpose] || esc(purpose)}：${usage.estimated?.total_tokens ? '≈ ' : ''}${tokenValue(usage.effective?.total_tokens ?? usage.total_tokens)} tokens · ${fmt(usage.requests)} 次请求${usage.reported.total_tokens < usage.requests ? '（统计不完整）' : ''}</p>`).join('')}<p class="hint">推理已包含在输出中，不重复相加。优先采用 API 或本地模型实际分词数；缺失字段单独标记估算，隐藏推理未知时显示已观测下限。累计消耗与下一轮上下文分别统计。</p><p>已整理 ${fmt(context.compactions)} 次 · 按 Token 预算自动压缩历史；长期记忆独立按每 ${fmt(context.memory_extract_tokens)} tokens 提取，最长等待 ${fmt(context.memory_extract_max_delay_seconds)} 秒。</p><p id="chat-summary" class="prewrap conversation-summary">${esc(context.summary || '对话摘要将在整理历史后显示。')}</p>`;
  }
  const lazyDetails = new Map();
  function lazyContent(key, render) {
    lazyDetails.set(key, render);
    return '<div data-lazy-content></div>';
  }
  function hydrateDetails(detail) {
    const body = $(':scope > [data-lazy-content]', detail);
    if (!body || (body.childNodes.length && !detail.detailsDirty)) return;
    body.innerHTML = lazyDetails.get(detail.dataset.detailKey)?.() || '';
    detail.detailsDirty = false;
    if (detail.rememberedOpen) restoreDetails(body, detail.rememberedOpen, detail.rememberedFocus);
  }
  document.addEventListener('toggle', event => {
    if (event.target.matches('details[data-detail-key]') && event.target.open && event.target.isConnected) hydrateDetails(event.target);
  }, true);
  function requestMarkup(request, live = false) {
    if (!request.reasoning) return '';
    const key = `reasoning:${request.id}`;
    return `<details class="chat-reasoning" data-detail-key="${esc(key)}"><summary><span class="reasoning-label">${live ? '正在思考' : '思考过程'}</span><span class="reasoning-model">${esc(request.model || '')}</span></summary>${lazyContent(key, () => `<div class="prewrap reasoning-body">${esc(request.reasoning)}</div>`)}</details>`;
  }
  const purposeLabels = { chat: '对话与工具循环', compression: '历史压缩', memory: '记忆提取', embedding: 'Embedding', reranker: 'Reranker', sticker: '表情包理解', persona_compile: '人格画像生成', persona_preview: '人格预览', persona_reflect: '自主反思', persona_validate: '进化验证' };
  function usageRequestMarkup(request) {
    const key = `usage-request:${request.id}`;
    return `<details class="usage-request" data-detail-key="${esc(key)}"><summary>${esc(purposeLabels[request.purpose] || request.purpose || '模型请求')} · ${esc(request.model || '模型')}${request.status === 'running' ? ' · 进行中' : request.status !== 'completed' ? ' · 未完成' : ''}</summary>${lazyContent(key, () => `<dl class="token-distribution request-usage" aria-label="模型请求 Token 用量">${tokenRows(request.usage)}</dl>${request.context ? `<details class="request-context" data-detail-key="request-context:${esc(request.id)}"><summary>输入分布（估算）</summary>${distributionMarkup(request.context)}</details>` : ''}<p class="hint">估算分词器：${esc(request.usage?.estimated?.tokenizer || '未记录')}</p>`)}</details>`;
  }
  function turnUsageMarkup(usage, requests = [], turnId = 'live') {
    if (!usage) return '';
    const summary = ['input_tokens', 'output_tokens', 'total_tokens'].map(key => {
      const { reported, estimates, value, source, missing, lower } = tokenMetric(usage, key);
      const display = !reported && !estimates && usage.requests !== 0 ? '未提供' : `${!estimates && (lower || missing) ? '≥ ' : ''}${tokenValue(value, estimates)}`;
      return `<span class="usage-metric" title="${esc(source)}${lower ? '，含已观测下限' : ''}"><span>${key === 'output_tokens' ? '输出' : tokenLabels[key]}</span> <span class="usage-value">${display}</span></span>`;
    }).join('<span class="usage-separator" aria-hidden="true">·</span>');
    const key = `usage:${turnId}`;
    return `<details class="turn-usage" data-detail-key="${esc(key)}" aria-label="本轮 Token 用量"><summary>${summary}</summary>${lazyContent(key, () => `<div class="usage-details"><p class="hint">本轮累计 · ${fmt(usage.requests)} 次模型请求${usage.incomplete_requests ? ` · ${fmt(usage.incomplete_requests)} 次未正常结束` : ''}</p><dl class="token-distribution" aria-label="本轮 Token 详细用量">${tokenRows(usage)}</dl><p class="hint">推理包含在输出中，缓存包含在输入中；实测与估算分别标记，未知分项不按零计入。超出浏览器精确整数范围的大数以 ≈ 显示。${usage.output_lower_bound_requests ? '部分输出仅包含已观测下限。' : ''}</p>${requests.length ? `<div class="usage-requests">${requests.map(usageRequestMarkup).join('')}</div>` : ''}</div>`)}</details>`;
  }
  function replaceDetails(container, markup) {
    const open = new Set($$('details[data-detail-key][open]', container).map(detail => detail.dataset.detailKey));
    const focused = container.contains(document.activeElement) && document.activeElement.tagName === 'SUMMARY' ? document.activeElement.parentElement.dataset.detailKey : null;
    container.innerHTML = markup;
    restoreDetails(container, open, focused);
  }
  function restoreDetails(container, open, focused) {
    for (const detail of $$('details[data-detail-key]', container)) {
      detail.rememberedOpen = open; detail.rememberedFocus = focused;
      detail.open = open.has(detail.dataset.detailKey);
      if (detail.open) hydrateDetails(detail);
      if (focused === detail.dataset.detailKey) $('summary', detail).focus({ preventScroll: true });
    }
  }
  function reasoningMarkup(requests) {
    requests = requests.filter(request => request.purpose === 'chat');
    return requests.map(request => `<div class="model-request" data-request-id="${esc(request.id)}">${requestMarkup(request, request.status === 'running')}</div>`).join('');
  }
  function answerMarkup(text) {
    const inline = value => value.split(/(`[^`\n]+`|\[[^\]\n]+\]\([^\s)]+\))/g).map(part => {
      if (part.startsWith('`')) return `<code>${esc(part.slice(1, -1))}</code>`;
      const link = part.match(/^\[([^\]]+)\]\((.+)\)$/);
      const url = link && safeLink(link[2]);
      if (url) return `<a href="${esc(url)}" target="_blank" rel="noreferrer noopener">${esc(link[1])}</a>`;
      return esc(part).replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>');
    }).join('');
    return String(text).split(/(```[\s\S]*?(?:```|$))/g).map(block => {
      if (block.startsWith('```')) {
        const lines = block.replace(/```$/, '').slice(3).split('\n');
        const language = lines.shift();
        return `<div class="code-block"><small>${esc(language || '代码')}</small><pre><code>${esc(lines.join('\n'))}</code></pre></div>`;
      }
      return block.split(/\n\s*\n/).filter(Boolean).map(paragraph => {
        const lines = paragraph.split('\n');
        if (lines.every(line => /^[-*] /.test(line))) return `<ul>${lines.map(line => `<li>${inline(line.slice(2))}</li>`).join('')}</ul>`;
        return `<p>${lines.map(line => /^#{1,4} /.test(line) ? `<strong class="answer-heading">${inline(line.replace(/^#{1,4} /, ''))}</strong>` : inline(line)).join('<br>')}</p>`;
      }).join('');
    }).join('');
  }
  function transcriptIndex(history) {
    const group = (rows, key) => {
      const groups = new Map();
      for (const row of rows || []) { const id = row[key]; if (!groups.has(id)) groups.set(id, []); groups.get(id).push(row); }
      return groups;
    };
    return { requests: group(history.model_requests, 'turn_id'), tools: group([...(history.tool_calls || [])].reverse(), 'turn_id'), assets: group(history.assets, 'message_id'), stickers: group(history.stickers, 'message_id') };
  }
  function messageState(history, message, index) {
    const assistant = message.role === 'assistant';
    return JSON.stringify([message, assistant && index.requests.get(message.turn_id), assistant && history.turn_usage?.[message.turn_id], assistant && index.tools.get(message.turn_id), index.assets.get(message.id), index.stickers.get(message.id)]);
  }
  function messageMarkup(history, message, index) {
    return `<article class="chat-message ${message.role}" data-message-id="${esc(message.id)}"><small>${message.role === 'user' ? '你' : 'Mneme'} · ${message.channel === 'qq' ? 'QQ' : '网页/API'} · ${date(message.created_at, true)}${message.status === 'failed' ? ' · 本轮未完成' : message.status === 'generating' ? ' · 正在回复' : ''}</small>${message.role === 'assistant' ? reasoningMarkup(index.requests.get(message.turn_id) || []) : ''}<div class="${message.role === 'assistant' ? 'answer-body' : 'prewrap'}">${message.role === 'assistant' ? answerMarkup(message.content) : esc(message.content)}</div><div class="chat-stickers">${(index.stickers.get(message.id) || []).map(s => `<img class="chat-sticker" src="/v1/sticker-images/${esc(s.image_id)}" alt="${esc(s.name)}" loading="lazy">`).join('')}</div><div class="chat-assets">${(index.assets.get(message.id) || []).map(a => `<button class="btn small" data-image="${esc(a.id)}">${message.role === 'assistant' ? '查看回复图片' : '查看上传图片'}</button>`).join('')}</div>${message.role === 'assistant' ? `<div class="chat-tools">${(index.tools.get(message.turn_id) || []).map(toolMarkup).join('')}</div>${turnUsageMarkup(history.turn_usage?.[message.turn_id], (index.requests.get(message.turn_id) || []), message.turn_id)}` : ''}</article>`;
  }
  function transcriptMarkup(history, index = transcriptIndex(history)) {
    return history.messages.map(message => messageMarkup(history, message, index)).join('') || `<div class="chat-welcome"><img src="/console/icon.svg" alt=""><h2>今天，想聊些什么？</h2><p>从这一刻开始，延续每一段交流。</p></div>`;
  }
  function syncTranscript(container, history, signatures) {
    const index = transcriptIndex(history);
    const existing = new Map($$('[data-message-id]', container).map(node => [node.dataset.messageId, node]));
    const ids = new Set(history.messages.map(message => message.id));
    for (const [id, node] of existing) if (!ids.has(id)) { node.remove(); signatures.delete(id); }
    if (!ids.size) { lazyDetails.clear(); container.innerHTML = transcriptMarkup(history, index); return; }
    $('.chat-welcome', container)?.remove();
    let previous = null;
    for (const message of history.messages) {
      const signature = messageState(history, message, index);
      let node = existing.get(message.id);
      if (!node || signatures.get(message.id) !== signature) {
        const open = new Set(node ? $$('details[data-detail-key][open]', node).map(detail => detail.dataset.detailKey) : []);
        const focused = node?.contains(document.activeElement) && document.activeElement.tagName === 'SUMMARY' ? document.activeElement.parentElement.dataset.detailKey : null;
        const template = document.createElement('template'); template.innerHTML = messageMarkup(history, message, index);
        const replacement = template.content.firstElementChild;
        if (node) node.replaceWith(replacement);
        node = replacement; restoreDetails(node, open, focused); signatures.set(message.id, signature);
      }
      if (node.previousElementSibling !== previous || node.parentElement !== container) container.insertBefore(node, previous ? previous.nextSibling : container.firstChild);
      previous = node;
    }
  }
  async function chatPage(epoch) {
    const [tools, history] = await Promise.all([api('/v1/console/tools'), api('/v1/console/messages')]); if (epoch !== state.epoch) return;
    const data = history.conversation;
    lazyDetails.clear();
    const index = transcriptIndex(history);
    const signatures = new Map(history.messages.map(message => [message.id, messageState(history, message, index)]));
    $('#page').innerHTML = `<section class="chat-panel" aria-label="与 Mneme 对话"><div id="chat-messages" class="chat-messages" role="log" aria-label="对话记录">${transcriptMarkup(history, index)}</div><div class="composer-wrap">${data.active_turn ? '<p class="chat-resuming" role="status">上一轮回复正在继续生成，完成后会自动显示。</p>' : ''}<form id="chat-form" class="chat-composer"><label class="sr-only" for="chat-message">消息</label><textarea id="chat-message" rows="1" maxlength="4000" placeholder="给 Mneme 发消息…"></textarea><div id="chat-attachments" class="chat-attachments"></div><div class="composer-actions"><label class="icon-btn image-upload" title="上传图片 · 最多 4 张，每张 8 MiB">${icon('plus')}<span class="sr-only">上传图片</span><input id="chat-files" type="file" aria-label="上传图片" accept="image/png,image/jpeg,image/webp" multiple></label><span class="composer-hint">Enter 发送 · Shift + Enter 换行</span><button class="send-button" id="send-chat" aria-label="发送" title="发送">${icon('arrow')}</button></div><div id="chat-error" class="error-inline" role="alert"></div></form><details class="chat-context-details"><summary id="chat-context">${esc(contextUsage(data.context))}</summary><div class="context-popover" id="chat-context-popover">${contextDetails(data.context)}</div></details></div></section>`;
    const uploads = []; let uploading = false;
    const composer = $('#chat-message');
    const updateComposer = () => {
      const locked = state.chatBusy || Boolean(data.active_turn) || uploading;
      composer.disabled = locked;
      $('#send-chat').disabled = locked || (!composer.value.trim() && !uploads.length);
      $('#chat-files').disabled = locked || !tools.enabled;
      $$('[data-remove-upload]').forEach(button => { button.disabled = locked; });
    };
    composer.oninput = () => { composer.style.height = 'auto'; composer.style.height = `${Math.min(composer.scrollHeight, 240)}px`; updateComposer(); };
    composer.onkeydown = event => { if (event.key === 'Enter' && !event.shiftKey && !event.isComposing && event.keyCode !== 229) { event.preventDefault(); if (!$('#send-chat').disabled) $('#chat-form').requestSubmit(); } };
    updateComposer();
    const renderUploads = () => { $('#chat-attachments').innerHTML = uploads.map((a, i) => `<span class="tag">图片 ${i + 1}<button type="button" class="icon-btn" data-remove-upload="${i}" aria-label="移除图片 ${i + 1}">${icon('close')}</button></span>`).join(''); $$('[data-remove-upload]').forEach(button => { button.onclick = () => { uploads.splice(Number(button.dataset.removeUpload), 1); renderUploads(); }; }); updateComposer(); };
    $('#chat-files').onchange = async event => {
      const files = [...event.target.files]; event.target.value = ''; const errorEl = $('#chat-error'); errorEl.textContent = '';
      if (files.length + uploads.length > 4 || files.some(file => file.size > 8 * 1024 * 1024)) { errorEl.textContent = '最多上传 4 张图片，每张不超过 8 MiB。'; return; }
      uploading = true; updateComposer();
      try { for (const file of files) { const asset = await api('/v1/images', { method: 'POST', body: file, headers: { 'Content-Type': 'application/octet-stream' } }); if (epoch !== state.epoch) return; uploads.push(asset.asset_id); renderUploads(); } }
      catch (error) { if (epoch === state.epoch && error.name !== 'AbortError') errorEl.textContent = '图片上传未完成，请检查图片格式与工具服务。'; }
      finally { uploading = false; if (epoch === state.epoch) updateComposer(); }
    };
    $('#chat-form').onsubmit = async event => {
      event.preventDefault(); if (state.chatBusy || data.active_turn || uploading) return;
      const message = $('#chat-message').value.trim() || (uploads.length ? '请查看我上传的图片。' : ''); if (!message) return;
      const assets = [...uploads]; const requestId = crypto.randomUUID(); const errorEl = $('#chat-error'); errorEl.textContent = ''; state.chatBusy = true;
      updateComposer();
      const messages = $('#chat-messages'); $('.chat-welcome', messages)?.remove();
      messages.insertAdjacentHTML('beforeend', `<article class="chat-message user"><small>你</small><div class="prewrap">${esc(message)}</div></article><article class="chat-message assistant" id="chat-current"><small>Mneme</small><div id="chat-live-requests"></div><div class="answer-body" id="chat-answer"></div><div class="chat-tools" id="chat-live-tools"></div><div id="chat-live-usage"></div><small id="chat-progress">正在思考…</small></article>`);
      messages.scrollTop = messages.scrollHeight;
      let persisted = false; let failed = false; let answer = ''; let liveTurnUsage; const liveRequests = new Map();
      let renderFrame = 0; let answerDirty = false; let usageDirty = false;
      const dirtyRequests = new Set(); const dirtyTools = new Map();
      const flushRender = () => {
        if (renderFrame) cancelAnimationFrame(renderFrame); renderFrame = 0;
        if (epoch !== state.epoch || (!answerDirty && !usageDirty && !dirtyRequests.size && !dirtyTools.size)) return;
        const follow = messages.scrollHeight - messages.scrollTop - messages.clientHeight < 100;
        for (const requestId of dirtyRequests) {
          const request = liveRequests.get(requestId);
          let element = document.getElementById(`request-${request.id}`);
          if (!element) { element = document.createElement('div'); element.className = 'model-request'; element.id = `request-${request.id}`; $('#chat-live-requests').append(element); }
          if (!$('.chat-reasoning', element)) element.innerHTML = requestMarkup(request, request.status === 'running');
          const detail = $('.chat-reasoning', element);
          if (detail) {
            const body = $('.reasoning-body', element);
            if (detail.open && body) body.textContent = request.reasoning;
            else detail.detailsDirty = true;
            $('.reasoning-label', element).textContent = request.status === 'running' ? '正在思考' : '思考过程';
            $('.reasoning-model', element).textContent = request.model || '';
          }
        }
        if (answerDirty) $('#chat-answer').innerHTML = answerMarkup(answer);
        for (const data of dirtyTools.values()) {
          const id = `tool-${data.id}`; let element = document.getElementById(id);
          if (!element) { element = document.createElement('div'); element.id = id; $('#chat-live-tools').append(element); }
          element.innerHTML = toolMarkup(data);
        }
        if (usageDirty) replaceDetails($('#chat-live-usage'), turnUsageMarkup(liveTurnUsage, [...liveRequests.values()], requestId));
        answerDirty = false; usageDirty = false; dirtyRequests.clear(); dirtyTools.clear();
        if (follow) messages.scrollTop = messages.scrollHeight;
      };
      const scheduleRender = () => { if (!renderFrame) renderFrame = requestAnimationFrame(flushRender); };
      try {
        const response = await fetch('/v1/chat', { method: 'POST', credentials: 'same-origin', cache: 'no-store', redirect: 'error', signal: state.controller.signal, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ request_id: requestId, message, assets }) });
        if (!response.ok) { if (response.status === 401) clearLogin(); throw new ApiFailure(response.status, (await response.json()).error); }
        const reader = response.body.getReader(); const decoder = new TextDecoder(); let buffer = '';
        while (true) {
          const { value, done } = await reader.read(); buffer += decoder.decode(value, { stream: !done }).replace(/\r\n/g, '\n');
          let split;
          while ((split = buffer.indexOf('\n\n')) >= 0) {
            const frame = buffer.slice(0, split); buffer = buffer.slice(split + 2);
            const name = frame.split('\n').find(line => line.startsWith('event:'))?.slice(6).trim();
            const raw = frame.split('\n').filter(line => line.startsWith('data:')).map(line => line.slice(5).trimStart()).join('\n'); if (!raw) continue;
            const data = JSON.parse(raw); if (name === 'done') persisted = data.persisted; if (name === 'error') failed = true;
            if (epoch !== state.epoch) continue;
            if (name === 'compaction') $('#chat-progress').textContent = data.status === 'running' ? '正在整理历史，延续对话…' : '历史已整理，正在思考…';
            if (name === 'context') {
              $('#chat-context').textContent = `当前请求 ≈ ${fmt(data.estimated_input_tokens)} / ${fmt(history.conversation.context.context_tokens)} tokens`;
            }
            if (name === 'usage' && data.usage.input_tokens != null) {
              $('#chat-context').textContent = `当前请求 ${fmt(data.usage.input_tokens)} / ${fmt(history.conversation.context.context_tokens)} tokens（API）`;
            }
            if (name === 'turn_usage') { liveTurnUsage = data.usage; usageDirty = true; }
            if (name === 'context' || name === 'reasoning' || name === 'usage') {
              const request = liveRequests.get(data.request_id) || { id: data.request_id, reasoning: '', usage: {}, status: 'running' };
              if (name === 'reasoning') request.reasoning += data.content;
              else if (name === 'usage') { request.usage = data.usage; request.status = data.status; }
              else { request.model = data.model; request.purpose = data.purpose; request.context = data.context; }
              liveRequests.set(request.id, request);
              dirtyRequests.add(request.id);
              if (name === 'usage' || name === 'context') usageDirty = true;
              if (name === 'reasoning') $('#chat-progress').textContent = '正在思考…';
            }
            if (name === 'delta') { answer += data.content; answerDirty = true; $('#chat-progress').textContent = '正在回复…'; }
            if (name === 'tool') { dirtyTools.set(data.id, data); $('#chat-progress').textContent = data.status === 'running' ? `正在${toolLabels[data.action] || '执行工具'}…` : '正在继续思考…'; }
            if (name === 'error') $('#chat-progress').textContent = '本轮失败，临时输出未保存。';
            scheduleRender();
          }
          if (done) break;
        }
        flushRender();
        if (!persisted) throw new Error(failed ? 'generation_failed' : 'stream_interrupted');
      } catch (error) { if (epoch === state.epoch && error.name !== 'AbortError') errorEl.textContent = `本轮未确认完成，可稍后刷新会话检查。${errorText(error)}`; }
      finally {
        flushRender();
        state.chatBusy = false;
        if (epoch === state.epoch && state.authenticated) {
          if (persisted) {
            const open = new Set($$('details[data-detail-key][open]', messages).map(detail => detail.dataset.detailKey));
            const follow = messages.scrollHeight - messages.scrollTop - messages.clientHeight < 100;
            const scrollTop = messages.scrollTop;
            const focused = messages.contains(document.activeElement) && document.activeElement.tagName === 'SUMMARY' ? document.activeElement.parentElement.dataset.detailKey : null;
            await route();
            if (state.epoch === epoch + 1 && state.route === 'chat' && $('#chat-messages')) {
              const container = $('#chat-messages');
              restoreDetails(container, open, focused);
              container.scrollTop = follow ? container.scrollHeight : scrollTop;
            }
          } else updateComposer();
        }
      }
    };
    $('#chat-messages').scrollTop = $('#chat-messages').scrollHeight;
    {
      const refreshTranscript = async () => {
        if (epoch !== state.epoch) return;
        if (state.chatBusy) { state.poll = setTimeout(refreshTranscript, 3000); return; }
        try {
          if (!document.hidden) {
            const latest = await api('/v1/console/messages');
            if (epoch !== state.epoch || state.chatBusy) return;
            data.active_turn = latest.conversation.active_turn;
            $('#chat-context').textContent = contextUsage(latest.conversation.context);
            $('#chat-context-popover').innerHTML = contextDetails(latest.conversation.context);
            if (!data.active_turn) $('.chat-resuming')?.remove();
            updateComposer();
            const container = $('#chat-messages');
            const atBottom = container.scrollHeight - container.scrollTop - container.clientHeight < 60;
            const top = container.getBoundingClientRect().top;
            const anchor = atBottom ? null : $$('[data-message-id]', container).find(node => node.getBoundingClientRect().bottom > top);
            const anchorTop = anchor?.getBoundingClientRect().top;
            syncTranscript(container, latest, signatures);
            if (atBottom) container.scrollTop = container.scrollHeight;
            else if (anchor) {
              const current = anchor.isConnected ? anchor : $$('[data-message-id]', container).find(node => node.dataset.messageId === anchor.dataset.messageId);
              if (current) container.scrollTop += current.getBoundingClientRect().top - anchorTop;
            }
          }
        } catch (error) { if (error.name === 'AbortError' || epoch !== state.epoch) return; }
        if (epoch === state.epoch && !state.chatBusy) state.poll = setTimeout(refreshTranscript, 3000);
      };
      state.poll = setTimeout(refreshTranscript, 3000);
    }
  }
  async function toolsPage(epoch) {
    const [status, data] = await Promise.all([api('/v1/console/tools'), api('/v1/console/tools/calls')]); if (epoch !== state.epoch) return;
    const service = status.service || {};
    $('#page').innerHTML = `<div class="tools-layout"><section class="panel job-card tools-status"><div class="between"><div><h2>mneme-tools</h2><p class="subheading">Tavily 网页搜索 · Chromium 网页浏览</p></div><span class="badge ${status.state === 'healthy' ? 'green' : 'amber'}">${status.enabled ? status.state === 'healthy' ? '可用' : '暂不可用' : '未启用'}</span></div><div class="system-facts"><div><small>浏览器会话</small><strong>${fmt(service.active_sessions)} / ${fmt(service.max_sessions)}</strong></div><div><small>每轮调用上限</small><strong>${fmt(status.max_calls)}</strong></div><div><small>调用 / 失败</small><strong>${fmt(status.metrics?.requests)} / ${fmt(status.metrics?.failures)}</strong></div><div><small>空闲回收</small><strong>${fmt((service.idle_timeout_seconds || 0) / 60)} 分</strong></div><div><small>浏览器沙箱</small><strong>${service.sandbox === true ? '已启用' : '不可用'}</strong></div><div><small>平均调用耗时</small><strong>${ms(status.metrics?.average_ms)}</strong></div></div><p class="hint">工具服务通过独立凭证与 Pod 内 localhost HTTP 连接。浏览器只允许访问公网 HTTP/HTTPS，图片由现有多模态 LLM 理解。</p></section><section class="panel tools-history"><div class="panel-head"><h2>最近工具调用</h2><p class="subheading">状态、耗时、错误与实际读取的来源，最多显示 100 条</p></div><div class="tool-log-list">${data.calls.map(call => `<details class="tool-log"><summary><span>${esc(toolLabels[call.action] || call.name)}</span><span class="badge ${call.status === 'failed' ? 'red' : call.status === 'running' ? 'amber' : 'green'}">${esc(call.status)}</span><small>${ms(call.elapsed_ms)} · ${date(call.created_at, true)}</small></summary><div class="tool-log-body">${toolMarkup(call)}<p class="hint">会话 ${esc(call.session_id)} · 轮次 ${esc(call.turn_id)}</p><button class="btn small" data-close-browser="${esc(call.session_id)}">释放浏览器会话</button><details class="technical"><summary>结构化结果</summary><pre class="prewrap">${esc(JSON.stringify(call.result, null, 2))}</pre></details></div></details>`).join('') || empty('还没有工具调用', '前往对话页，让 Mneme 搜索网页、阅读资料或查看图片。', '<a class="btn" href="#chat">开始对话</a>')}</div></section></div>`;
    $$('[data-close-browser]').forEach(button => { button.onclick = async () => { button.disabled = true; try { await send('/v1/console/tools/close', { session_id: button.dataset.closeBrowser }); toast('浏览器会话已释放。'); route(); } catch (error) { toast(errorText(error), true); button.disabled = false; } }; });
    if (data.calls.some(call => call.status === 'running')) state.poll = setTimeout(() => { if (epoch === state.epoch) route(); }, 3000);
  }

  function qqCard(qq) {
    return `<section class="panel job-card spaced"><div class="between"><div><h2>QQ 机器人</h2><p class="subheading">${qq.enabled ? `AppID ${esc(qq.app_id)} · 个人单聊` : '在 .env 填写 QQ AppID 和 AppSecret 后启用'}</p></div><span class="badge ${qq.has_session ? 'green' : 'amber'}">${qq.has_session ? '个人会话已建立' : qq.bound ? '等待新会话' : qq.enabled ? '等待首条私聊' : '未启用'}</span></div>${qq.enabled ? `<p class="hint spaced">直接私聊即可开始，无需绑定码。首次私聊自动建立个人会话，重启后继续使用。</p><div class="spaced"><small>待处理 ${fmt(qq.queue.pending)} · 运行中 ${fmt(qq.queue.running)} · 已发送 ${fmt(qq.queue.sent)} · 失败 ${fmt(qq.queue.dead)}</small></div>` : ''}</section>`;
  }
  function collapsibleSettings(panel, key) {
    const head = $(':scope > .panel-head, :scope > .between', panel);
    const summary = document.createElement('summary');
    summary.className = head.className; summary.classList.add('panel-head');
    const button = $(':scope > button', head);
    if (button) {
      const actions = document.createElement('div'); actions.className = 'settings-panel-actions';
      actions.append(button); head.after(actions);
    }
    summary.append(...head.childNodes); head.replaceWith(summary);
    if (panel.tagName !== 'DETAILS') {
      const details = document.createElement('details');
      details.className = `${panel.className} settings-disclosure`; details.id = panel.id;
      details.append(...panel.childNodes); panel.replaceWith(details); panel = details;
      panel.open = state.agentPanels[key] ?? false;
    }
    panel.dataset.setting = key;
    panel.ontoggle = () => { if (panel.isConnected) state.agentPanels[key] = panel.open; };
    return panel;
  }
  const stickerAnalysisError = code => ({
    catalog_full: '表情库上下文容量不足，请移除部分表情后重试。',
    incomplete_image_description: '模型输出被截断，请重试识别。图片已保留。',
    invalid_image_description: '模型描述超出格式或长度限制，请重试识别或手动修正。图片已保留。',
    invalid_json: '模型未返回有效的识别格式，请重试识别。图片已保留。',
    empty_image_description: '模型未返回描述，请重试识别。图片已保留。',
    timeout: '识别请求超时，请稍后重试。图片已保留。',
  })[code] || '自动识别未完成，请检查模型或工具服务后重试。图片已保留。';
  async function stickerPanel(epoch) {
    const data = await api('/v1/stickers'); if (epoch !== state.epoch) return;
    let panel = $('#sticker-panel');
    panel.innerHTML = `<div class="panel-head between"><div><h2>自定义表情包</h2><p class="subheading">你收藏的表情，让每次回应多一点默契。</p></div><button class="btn primary" id="add-sticker" ${data.upload_available ? '' : 'disabled'}>${icon('plus')}添加表情包</button></div><div class="sticker-body"><label class="check-label"><input type="checkbox" id="stickers-enabled" ${data.enabled ? 'checked' : ''}>允许 Agent 在回复中使用表情包</label><p class="hint">网页、QQ 和 API 共用这份表情库。上传后自动理解画面、文案和情绪，按语境选用，每轮最多一张。重启和重置对话后保留。</p>${!data.upload_available ? '<div class="notice warn">启用工具服务后即可上传图片；已有表情包仍可使用。</div>' : ''}<div class="sticker-toolbar"><label class="search-field">${icon('search')}<input id="sticker-search" placeholder="搜索名字或使用场景" aria-label="搜索表情包"></label><small id="sticker-count"></small></div><div id="sticker-grid" class="sticker-grid"></div><div class="error-inline" id="stickers-error" role="alert"></div><p class="hint">支持 PNG、JPEG、WebP 和 GIF 动图，原图最多 8 MiB。最多 ${data.limit} 张；自动缩至 512 像素并移除元数据。</p></div>`;
    panel = collapsibleSettings(panel, 'stickers');
    const render = () => {
      const q = $('#sticker-search').value.trim().toLocaleLowerCase();
      const visible = data.stickers.filter(s => `${s.name} ${s.description}`.toLocaleLowerCase().includes(q));
      $('#sticker-count').textContent = `${data.stickers.length} / ${data.limit} 张`;
      $('#sticker-grid').innerHTML = visible.map(s => `<article class="sticker-card${s.enabled ? '' : ' is-disabled'}"><div class="sticker-preview"><img src="/v1/sticker-images/${esc(s.image_id)}" alt="${esc(s.name)}" loading="lazy"></div><div class="sticker-caption"><strong>${esc(s.name)}</strong><span class="badge ${s.enabled ? 'green' : ''}">${s.analysis_status === 'pending' ? '正在识别' : s.analysis_status === 'failed' ? '待重试' : s.enabled ? '已启用' : '已停用'}</span></div><p>${esc(s.analysis_status === 'pending' ? '正在自动理解画面和表达，无需手动打标签…' : s.analysis_status === 'failed' ? stickerAnalysisError(s.analysis_error) : s.description)}</p><div class="sticker-actions"><button class="btn small" data-sticker-analyze="${esc(s.id)}" ${s.analysis_status === 'pending' ? 'disabled' : ''}>${s.analysis_status === 'failed' ? '重试识别' : '重新识别'}</button><button class="btn small" data-sticker-edit="${esc(s.id)}" aria-label="编辑 ${esc(s.name)}">${icon('edit')}编辑</button><button class="icon-btn" data-sticker-delete="${esc(s.id)}" aria-label="删除 ${esc(s.name)}">${icon('trash')}</button></div></article>`).join('') || `<div class="sticker-empty">${icon('spark')}<strong>${q ? '没有找到表情包' : '把你的表情收藏放在这里'}</strong><p>${q ? '试试其他名字或使用场景。' : '上传喜欢的表情，Agent 会自动理解什么时候用它。'}</p></div>`;
      $$('[data-sticker-analyze]', panel).forEach(button => { button.onclick = async () => {
        button.disabled = true; $('#stickers-error').textContent = '';
        try { await send(`/v1/stickers/${button.dataset.stickerAnalyze}/analyze`, {}); if (epoch === state.epoch) await stickerPanel(epoch); }
        catch (error) { if (error.name !== 'AbortError' && epoch === state.epoch) $('#stickers-error').textContent = errorText(error); button.disabled = false; }
      }; });
      $$('[data-sticker-edit]', panel).forEach(button => { button.onclick = () => editSticker(data.stickers.find(s => s.id === button.dataset.stickerEdit), epoch); });
      $$('[data-sticker-delete]', panel).forEach(button => { button.onclick = () => {
        const sticker = data.stickers.find(s => s.id === button.dataset.stickerDelete);
        const dialog = openDialog('confirm', `<header class="dialog-head"><h2>删除表情包？</h2><button class="icon-btn" data-close="confirm" aria-label="关闭">${icon('close')}</button></header><div class="dialog-body"><p>将“${esc(sticker.name)}”从表情库移除，已发送的历史图片会保留。</p><div class="error-inline" role="alert"></div></div><footer class="dialog-footer"><button class="btn" data-close="confirm">取消</button><button class="btn danger" id="confirm-sticker-delete">确认删除</button></footer>`);
        $('#confirm-sticker-delete', dialog).onclick = async event => {
          const button = event.currentTarget; button.disabled = true;
          try { await api(`/v1/stickers/${sticker.id}`, { method: 'DELETE' }); closeDialog('confirm'); await stickerPanel(epoch); toast('表情包已删除。'); }
          catch (error) { if (error.name !== 'AbortError') $('.error-inline', dialog).textContent = errorText(error); button.disabled = false; }
        };
      }; });
    };
    render(); $('#sticker-search').oninput = render;
    const pollAnalysis = async () => {
      try { const latest = await api('/v1/stickers'); if (epoch !== state.epoch) return; data.stickers = latest.stickers; render(); }
      catch (error) { if (error.name === 'AbortError' || epoch !== state.epoch) return; }
      if (data.stickers.some(s => s.analysis_status === 'pending')) state.poll = setTimeout(pollAnalysis, 2000);
    };
    clearTimeout(state.poll);
    if (data.stickers.some(s => s.analysis_status === 'pending')) state.poll = setTimeout(pollAnalysis, 2000);
    $('#add-sticker').onclick = () => editSticker(null, epoch);
    $('#stickers-enabled').onchange = async event => {
      const input = event.target; input.disabled = true; $('#stickers-error').textContent = '';
      try { await send('/v1/stickers/settings', { enabled: input.checked }, 'PUT'); toast(input.checked ? '已开启全局表情包。' : '已暂停使用表情包。'); }
      catch (error) { input.checked = !input.checked; if (error.name !== 'AbortError' && epoch === state.epoch) $('#stickers-error').textContent = errorText(error); }
      finally { input.disabled = false; }
    };
  }
  function editSticker(sticker, epoch) {
    const dialog = openDialog('editor', `<form id="sticker-form"><header class="dialog-head"><div><div class="eyebrow">YOUR EXPRESSIONS</div><h2>${sticker ? '编辑表情包' : '添加你的表情包'}</h2></div><button type="button" class="icon-btn" data-close="editor" aria-label="关闭">${icon('close')}</button></header><div class="dialog-body stack">${sticker ? '' : '<label class="field">选择图片<input type="file" id="sticker-files" accept="image/png,image/jpeg,image/webp,image/gif" multiple required><span class="hint">可一次选择多张，每张不超过 8 MiB；GIF 会保留动效。</span></label>'}<div id="sticker-editor-items" class="stack"></div><p class="hint">${sticker ? '下面是识别结果，可按需要修正；保存后采用你的描述。' : '上传后自动识别画面、文案、情绪和使用场景，无需填写标签。识别会在后台继续，关闭页面也不影响。'}</p><div class="error-inline" id="sticker-editor-error" role="alert"></div></div><footer class="dialog-footer"><button type="button" class="btn" data-close="editor">取消</button><button type="submit" class="btn primary">${sticker ? '保存表情包' : '上传表情包'}</button></footer></form>`);
    let files = []; let previews = [];
    const fields = (name, description, src, enabled = true) => `<div class="sticker-editor-item"><img src="${esc(src)}" alt="表情预览"><div class="stack"><label class="field">名字<input name="sticker_name" maxlength="40" required value="${esc(name)}"></label><label class="field">识别描述（可选修正）<textarea name="sticker_description" maxlength="120" rows="2" placeholder="例如：开心、庆祝、打招呼">${esc(description)}</textarea></label><label class="check-label"><input name="sticker_enabled" type="checkbox" ${enabled ? 'checked' : ''}>允许使用这张表情</label></div></div>`;
    if (sticker) $('#sticker-editor-items').innerHTML = fields(sticker.name, sticker.description, `/v1/sticker-images/${sticker.image_id}`, sticker.enabled);
    else $('#sticker-files').onchange = event => {
      previews.forEach(url => { URL.revokeObjectURL(url); objectURLs.delete(url); }); previews = [];
      files = [...event.target.files]; const error = $('#sticker-editor-error'); error.textContent = '';
      if (files.length > 50 || files.some(f => f.size > 8 * 1024 * 1024 || !['image/png', 'image/jpeg', 'image/webp', 'image/gif'].includes(f.type))) { files = []; event.target.value = ''; error.textContent = '请选择 PNG、JPEG、WebP 或 GIF，每张不超过 8 MiB，最多 50 张。'; }
      $('#sticker-editor-items').innerHTML = files.map(file => { const url = URL.createObjectURL(file); objectURLs.add(url); previews.push(url); return `<div class="sticker-editor-item"><img src="${esc(url)}" alt="表情预览"><div class="stack"><strong>${esc(file.name)}</strong><p class="hint">上传后自动识别</p><label class="check-label"><input name="sticker_enabled" type="checkbox" checked>识别完成后允许使用</label></div></div>`; }).join('');
    };
    $('#sticker-form').onsubmit = async event => {
      event.preventDefault(); const form = event.target; const button = $('button[type=submit]', form); const error = $('#sticker-editor-error'); error.textContent = '';
      const items = $$('.sticker-editor-item', form).map(el => ({ name: $('[name=sticker_name]', el)?.value.trim() || '', description: $('[name=sticker_description]', el)?.value.trim() || '', enabled: $('[name=sticker_enabled]', el).checked }));
      if (!items.length || (sticker && items.some(item => !item.name))) return;
      $$('input,textarea,button', form).forEach(el => { el.disabled = true; });
      let completed = 0;
      try {
        if (sticker) await send(`/v1/stickers/${sticker.id}`, items[0], 'PUT');
        else for (let i = 0; i < files.length; i++) { button.textContent = `正在上传 ${i + 1} / ${files.length}…`; await api(`/v1/stickers?${new URLSearchParams({ enabled: items[i].enabled })}`, { method: 'POST', body: files[i], headers: { 'Content-Type': 'application/octet-stream' } }); completed++; }
        previews.forEach(url => { URL.revokeObjectURL(url); objectURLs.delete(url); });
        closeDialog('editor'); if (epoch === state.epoch) { await stickerPanel(epoch); toast(sticker ? '表情包已保存。' : `已添加 ${completed} 张表情包，正在后台自动识别。`); }
      } catch (failure) {
        if (failure.name !== 'AbortError' && epoch === state.epoch) {
          error.textContent = `${completed ? `已上传 ${completed} 张，剩余图片未完成。` : ''}${errorText(failure)}`;
          if (completed) { files = files.slice(completed); $$('.sticker-editor-item', form).slice(0, completed).forEach(el => el.remove()); await stickerPanel(epoch); }
        }
        $$('input,textarea,button', form).forEach(el => { el.disabled = false; }); button.textContent = sticker ? '保存表情包' : '上传剩余表情包';
      }
    };
  }
  async function evolutionPanel(host, epoch, changed) {
    let data, busy = false;
    const path = '/v1/console/persona/evolution';
    const areas = { communication: '交流习惯', preferences: '个人偏好', initiative: '主动性', emotion: '情绪表达', task: '任务处理', tools: '工具策略', memory: '记忆关注' };
    const alive = () => epoch === state.epoch && host.isConnected;
    const labels = { applied: '已应用', rejected: '未通过 · 保留旧版', no_change: '无需修改', needs_owner: '核心变更建议 · 未应用', conflict: '配置变化 · 未应用', rollback: '用户回退', owner_control: '用户调整' };
    const render = () => {
      if (!alive()) return;
      const s = data.state;
      host.innerHTML = `<details class="evolution-panel" open><summary><strong>自主进化</strong><span class="badge ${data.status === 'ready' ? 'green' : 'amber'}" id="evolution-status">${esc({ ready: '进化记忆已生效', empty: '尚无自我形成的习惯', stale: '人格已变更 · 进化记忆待复核' }[data.status])}</span></summary>
        <p class="hint">只学习有真实互动依据的软行为倾向，不改核心人格、权限或事实。自动反思至少间隔一小时且积累八轮新对话；暂停只停止学习，现有有效习惯仍参与回复。版本 ${fmt(s.version)}${s.updated_at ? ` · ${date(s.updated_at, true)}` : ''} · 待评估线索 ${fmt(data.pending)}</p>
        ${s.error ? `<p class="notice warn">${esc({ reflection_rejected: '上次反思未通过检查，旧版仍保留。', reflection_interrupted: '上次反思被重启中断，未完成版本没有生效。', evolution_cache_invalid: '进化缓存无法读取，当前停用该层；历史仍可查看。' }[s.error] || s.error)}</p>` : ''}
        <form id="evolution-control"><label class="check-label"><input name="paused" type="checkbox"${s.paused ? ' checked' : ''}>暂停自主进化</label>
          ${s.notes.map(n => `<div class="evolution-note"><div class="between"><strong>${esc(areas[n.area] || n.area)}</strong><label class="check-label"><input name="locked" type="checkbox" value="${esc(n.id)}"${n.locked ? ' checked' : ''}>锁定</label></div><p>${esc(n.text)}</p><details><summary>真实来源</summary>${n.evidence.map(e => `<p><q>${esc(e.quote)}</q><br><small>${esc(e.source_id)}</small></p>`).join('')}</details></div>`).join('') || '<p class="hint">有明确反馈时才反思，不要求每轮调整。</p>'}
          <button class="btn" type="submit"${busy || s.running ? ' disabled' : ''}>保存进化设置</button></form>
        <details class="evolution-reflection"><summary>选择真实反馈，反思一次</summary><p class="hint">使用已保存的人格，选择最多八条已完成用户消息。手动操作会跳过自动冷却；有修改时包含一次反思和一次独立验证调用。验证是模型审查与基础能力哨兵，不能保证自然度；预览不进入证据。</p><form id="evolution-reflect">${data.sources.map(m => `<label class="evolution-source check-label"><input type="checkbox" name="source" value="${esc(m.id)}"><span>${esc(m.content.slice(0, 320))}<small>${date(m.created_at, true)}</small></span></label>`).join('') || '<p class="hint">暂无真实对话，请先完成一次交流。</p>'}<button class="btn" type="submit"${busy || s.running || s.paused || !data.sources.length ? ' disabled' : ''}>反思并验证</button></form></details>
        <div class="error-inline" id="evolution-error" role="alert"></div>${s.running || busy ? '<p class="hint" role="status">正在反思或验证，当前仍使用已生效版本…</p>' : ''}
        <details class="evolution-history"><summary>修改历史、差异与验证（最近三十条）</summary>${data.history.map(r => `<details data-evolution-revision="${r.version}"><summary>版本 ${fmt(r.version)} · ${esc(labels[r.status] || r.status)} · ${date(r.at, true)}</summary><p>${esc(r.reason || '')}</p><details><summary>触发证据</summary><pre class="prewrap">${esc(JSON.stringify(r.evidence || [], null, 2))}</pre></details><div class="evolution-diff"><div><small>修改前</small><pre class="prewrap">${esc(JSON.stringify(r.before, null, 2))}</pre></div><div><small>修改后 / 候选</small><pre class="prewrap">${esc(JSON.stringify(r.proposed ?? r.after, null, 2))}</pre></div></div><details><summary>验证结果${r.validation?.passed === true ? ' · 通过' : r.validation?.skipped ? ' · 未运行 / 不适用' : ' · 未通过'}</summary><pre class="prewrap">${esc(JSON.stringify(r.validation, null, 2))}</pre></details><small>${esc(r.model || '')} · ${esc(r.base_hash || r.source_hash || '')}</small>${['applied', 'rollback', 'owner_control'].includes(r.status) ? `<button class="btn small" type="button" data-evolution-rollback="${r.version}">回退到此版</button>` : ''}</details>`).join('') || '<p class="hint">暂无记录。</p>'}</details>
        <button class="btn small" type="button" data-evolution-rollback="0"${busy || s.running ? ' disabled' : ''}>清空进化层并记录回退</button></details>`;
      const mutate = async (suffix, body, method = 'POST') => {
        busy = true; const buttons = $$('button', host).map(b => [b, b.disabled]); buttons.forEach(([b]) => b.disabled = true);
        try { data = await send(path + suffix, { expected_version: data.state.version, ...body }, method); await changed(); }
        catch (error) { if (alive()) $('#evolution-error', host).textContent = errorText(error); return; }
        finally { busy = false; if (alive()) buttons.forEach(([b, disabled]) => b.disabled = disabled); }
        render();
      };
      $('#evolution-control', host).onsubmit = event => { event.preventDefault(); mutate('', { paused: event.currentTarget.elements.paused.checked, locked: $$('[name="locked"]:checked', host).map(n => n.value) }, 'PUT'); };
      $('#evolution-reflect', host).onsubmit = event => {
        event.preventDefault(); const ids = $$('[name="source"]:checked', host).map(n => n.value);
        if (!ids.length || ids.length > 8) { $('#evolution-error', host).textContent = '请选择 1–8 条真实用户消息。'; return; }
        mutate('/reflect', { source_ids: ids });
      };
      $$('[data-evolution-rollback]', host).forEach(b => b.onclick = () => mutate('/rollback', { target_version: Number(b.dataset.evolutionRollback) }));
    };
    try { data = await api(path); render(); } catch (error) { if (alive()) host.textContent = errorText(error); return; }
    const poll = async () => { if (!alive() || busy || !data.state.running) return; try { data = await api(path); render(); if (!data.state.running) await changed(); } catch (_) {} if (alive() && data.state.running) setTimeout(poll, 1500); };
    if (data.state.running) setTimeout(poll, 1500);
  }

  async function derivationPanel(epoch) {
    const titles = { overall: '整体行为倾向', chat: '日常闲聊', emotion: '情绪交流', task: '任务处理', tools: '工具使用', memory: '记忆关注与表达' };
    const labels = { empty: '尚未生成', ready: '已生效', disabled: '已停用 · 仅使用原始人格', stale: '原始人格已修改 · 待更新', needs_review: '保留了人工内容 · 待核对' };
    let data = await api('/v1/console/persona/derivation');
    if (epoch !== state.epoch) return () => {};
    let dirty = false, busy = false, showSignals = false, enabledDraft = null;
    const release = new Set();
    const panel = collapsibleSettings($('#persona-derivation'), 'derivation');
    const content = $('.derivation-content', panel);
    const rawUnsaved = () => Object.entries(data.effective.persona).some(([key, value]) => $('#persona-form').elements[key]?.value !== value);
    const dimensions = () => Object.keys(titles).map(id => data.profile?.dimensions.find(d => d.id === id) || { id, description: '', basis: '', evidence: [], signals: [], locked: false, edited: false });
    const collect = () => dimensions().map(d => ({ ...d, description: $(`[name="description-${d.id}"]`, content).value, basis: $(`[name="basis-${d.id}"]`, content).value, signals: $(`[name="signals-${d.id}"]`, content).value.split(/[,，\n]/).map(s => s.trim()).filter(Boolean), locked: $(`[name="lock-${d.id}"]`, content).checked }));
    const fail = error => { if (error.name !== 'AbortError') $('#derivation-error', content).textContent = errorText(error); };
    const token = () => ({ expected_version: data.version, source_hash: data.source_hash });
    const checkSaved = () => { if (rawUnsaved()) { toast('请先保存上方原始人格，再生成或预览。', true); return false; } return true; };
    const render = drafts => {
      if (epoch !== state.epoch) return;
      const entries = drafts || dimensions();
      content.innerHTML = `<div class="derivation-meta between"><span class="badge ${data.status === 'ready' ? 'green' : 'amber'}" id="derivation-status">${esc(labels[data.status])}${data.generation.running || busy ? ' · 正在生成' : ''}</span><small>版本 ${fmt(data.version)}${data.profile ? ` · ${date(data.profile.updated_at, true)} · ${esc(data.profile.model)}` : ''}</small></div>
        <p class="hint">原始人格始终优先。画像是可调整的解释，六个维度不对应固定话术。修改原始人格后，待更新画像暂不参与回复；人工编辑和锁定项不会被重新生成覆盖。</p>
        ${data.generation.error ? `<div class="notice warn">${esc({ generation_interrupted: '上次生成被服务重启中断，可重新生成。', persona_cache_invalid: '画像缓存无法读取，当前仅使用原始人格；可重新生成。', persona_generation_failed: '生成失败或证据格式不合格，原画像已保留。', persona_derivation_changed: '生成期间配置发生变化，生成结果未覆盖现有内容。', persona_derivation_budget: '画像超出上下文预算，请缩短内容。' }[data.generation.error] || '生成未完成，原画像已保留。')}</div>` : ''}
        <form id="derivation-form"><label class="check-label"><input type="checkbox" name="enabled"${(enabledDraft ?? data.profile?.enabled ?? true) ? ' checked' : ''}>启用画像（取消后仅使用原始人格）</label><div class="derivation-dimensions">${entries.map(d => `<details class="derivation-dimension" data-dimension="${d.id}"><summary><strong>${titles[d.id]}</strong><span class="badge">${release.has(d.id) ? '允许 AI 更新' : d.edited ? '人工编辑' : 'AI 衍生'}${d.locked ? ' · 已锁定' : ''}</span></summary><div class="derivation-fields"><label class="field">行为倾向<textarea name="description-${d.id}" maxlength="300">${esc(d.description)}</textarea></label><label class="field">推导依据 / 调整说明<textarea name="basis-${d.id}" maxlength="150">${esc(d.basis)}</textarea></label>${d.evidence.length ? `<div class="hint">原始依据：${d.evidence.map(e => `<q>${esc(e.quote)}</q>（${esc(e.field)}）`).join('；')}</div>` : ''}<label class="field">可选参考标签<input name="signals-${d.id}" value="${esc((d.signals || []).join('，'))}" maxlength="74" placeholder="最多 3 个定性标签，以逗号分隔"><span class="hint">仅用于可视化，不作为概率、阈值或运行时规则。</span></label><div class="between"><label class="check-label"><input type="checkbox" name="lock-${d.id}"${d.locked ? ' checked' : ''}>锁定此项，生成时保留</label><button type="button" class="btn small" data-release="${d.id}">允许 AI 更新此项</button></div></div></details>`).join('')}</div>
        <label class="check-label"><input type="checkbox" id="show-persona-signals"${showSignals ? ' checked' : ''}>显示参考标签</label><div id="persona-signals" class="entity-list"${showSignals ? '' : ' hidden'}>${entries.flatMap(d => (d.signals || []).map(s => `<span class="tag">${titles[d.id]} · ${esc(s)}</span>`)).join('') || '<small>尚未设置参考标签。</small>'}</div>
        <div class="error-inline" id="derivation-error" role="alert"></div><div class="settings-actions"><button class="btn" type="button" id="generate-derivation"${busy || data.generation.running ? ' disabled' : ''}>${data.profile ? '重新生成' : '生成行为画像'}</button><button class="btn primary" type="submit"${busy ? ' disabled' : ''}>核对并保存画像</button></div></form>
        <details class="derivation-effective"><summary>实际生效的完整人格配置</summary><p class="hint">包含当前原始人格、画像生效状态及正式对话的系统提示；每轮历史、召回记忆和工具结果按语境另行加入。</p><pre class="prewrap">${esc(JSON.stringify({ ...data.effective, source_hash: data.source_hash, version: data.version }, null, 2))}</pre></details>
        <details class="derivation-history"><summary>最近版本与回退（最多五版）</summary>${data.history.length ? data.history.slice().reverse().map(p => `<details><summary>版本 ${fmt(p.version)} · ${date(p.updated_at, true)}</summary><pre class="prewrap">${esc(JSON.stringify(p, null, 2))}</pre><button class="btn small" data-rollback="${p.version}">恢复此版${p.source_hash !== data.source_hash ? '为待核对草稿' : ''}</button></details>`).join('') : '<p class="hint">保存或生成下一版后，上一版会保留在这里。</p>'}</details>
        <div class="evolution-host"></div><form id="persona-preview" class="derivation-preview"><h3>人格一致性预览</h3><label class="field">示例消息<textarea name="message" maxlength="4000" required placeholder="输入一条日常分享、问题或需要表达观点的消息…"></textarea></label><label class="check-label"><input type="checkbox" name="use_context">参考当前对话与真实记忆</label><p class="hint">使用已保存且当前生效的配置。点击预览才调用一次生成模型；不保存对话、不提取记忆，也不执行外部工具。参考记忆时会运行现有检索。</p><div class="settings-actions"><button class="btn" type="submit">预览回复</button></div><div class="error-inline" id="preview-error" role="alert"></div><div class="prewrap preview-answer" id="preview-answer" aria-live="polite"></div></form>`;
      evolutionPanel($('.evolution-host', content), epoch, async () => { const latest = await api('/v1/console/persona/derivation'); if (epoch !== state.epoch) return; data.effective = latest.effective; $('.derivation-effective pre', content).textContent = JSON.stringify({ ...data.effective, source_hash: data.source_hash, version: data.version }, null, 2); });
      $('#derivation-form', content).oninput = event => { if (event.target.id === 'show-persona-signals') return; dirty = true; enabledDraft = event.currentTarget.elements.enabled.checked; };
      $('#show-persona-signals', content).onchange = event => { showSignals = event.target.checked; $('#persona-signals', content).hidden = !showSignals; };
      $$('[data-release]', content).forEach(button => { button.onclick = () => { const drafts = collect(); release.add(button.dataset.release); const d = drafts.find(d => d.id === button.dataset.release); d.locked = false; dirty = true; render(drafts); }; });
      $('#derivation-form', content).onsubmit = async event => {
        event.preventDefault(); if (!checkSaved()) return;
        const payload = { ...token(), enabled: event.currentTarget.elements.enabled.checked, dimensions: collect(), release: [...release] };
        const button = $('button[type=submit]', event.currentTarget); button.disabled = true;
        try { data = await send('/v1/console/persona/derivation', payload, 'PUT'); dirty = false; enabledDraft = null; release.clear(); render(); toast('画像已保存，下一轮对话生效。'); } catch (error) { fail(error); } finally { if (button.isConnected) button.disabled = false; }
      };
      $('#generate-derivation', content).onclick = async () => {
        if (!checkSaved()) return;
        if (dirty) { toast('请先保存画像编辑，再重新生成。', true); return; }
        busy = true; render();
        try { data = await send('/v1/console/persona/derivation/generate', { ...token(), force: true }); }
        catch (error) { fail(error); }
        finally { busy = false; if (epoch === state.epoch) { const errorText = $('#derivation-error', content)?.textContent; render(); $('#derivation-error', content).textContent = errorText || ''; } }
      };
      $$('[data-rollback]', content).forEach(button => { button.onclick = async () => {
        if (!checkSaved()) return;
        if (dirty) { toast('请先保存画像编辑，再回退。', true); return; }
        try { data = await send('/v1/console/persona/derivation/rollback', { ...token(), target_version: Number(button.dataset.rollback) }); render(); toast('已恢复所选画像；待更新或待核对版本不会自动生效。'); } catch (error) { fail(error); }
      }; });
      $('#persona-preview', content).onsubmit = async event => {
        event.preventDefault(); if (!checkSaved()) return;
        if (dirty) { toast('请先保存画像编辑，再预览。', true); return; }
        const form = event.currentTarget; const button = $('button[type=submit]', form); button.disabled = true;
        $('#preview-error', content).textContent = ''; $('#preview-answer', content).textContent = '正在生成预览…';
        try { const result = await send('/v1/console/persona/preview', { ...token(), expected_evolution_version: data.effective.evolution_version, message: form.elements.message.value, use_context: form.elements.use_context.checked }); if (epoch === state.epoch) $('#preview-answer', content).textContent = result.version === data.version && result.source_hash === data.source_hash && result.evolution_version === data.effective.evolution_version ? result.reply : '配置已变化，请使用当前版本重新预览。'; }
        catch (error) { if (error.name !== 'AbortError' && epoch === state.epoch) { $('#preview-error', content).textContent = errorText(error); $('#preview-answer', content).textContent = ''; } }
        finally { if (button.isConnected) button.disabled = false; }
      };
    };
    const refresh = async () => { const drafts = dirty ? collect() : null; const latest = await api('/v1/console/persona/derivation'); if (epoch !== state.epoch) return; if (dirty && latest.version !== data.version) { toast('画像有新版本，当前草稿已保留；请刷新并核对后保存。', true); return; } data = latest; render(drafts); };
    render();
    const poll = async () => { if (epoch !== state.epoch || !data.generation.running) return; try { if (!dirty) await refresh(); } catch (error) { fail(error); } if (epoch === state.epoch && data.generation.running) setTimeout(poll, 1500); };
    if (data.generation.running) setTimeout(poll, 1500);
    return refresh;
  }

  async function agentPage(epoch) {
    const [data, qq, conversation] = await Promise.all([api('/v1/console/settings'), api('/v1/qq/status'), api('/v1/conversation')]);
    if (epoch !== state.epoch) return;
    const fields = [['name', '名字', 80, '希望怎样称呼 Agent'], ['description', '性格描述', 6000, '身份、个性与陪伴方式'], ['style', '交流方式', 6000, '语气、表达习惯和回复偏好'], ['boundaries', '边界', 6000, '需要遵守的约定与能力边界'], ['output_format', '输出格式 Prompt', 6000, '例如：使用纯文本，分段回复，不使用 Markdown。留空表示不额外约束输出格式。']];
    $('#page').innerHTML = `<div class="agent-layout"><section class="panel agent-profile"><div class="agent-avatar">${icon('leaf')}</div><div><h2 id="agent-name">${esc(data.persona.name)}</h2><p>${esc(data.model)}</p></div><span class="badge green">全局共享对话</span></section><div class="agent-columns"><div class="settings-stack"><section class="panel" id="sticker-panel"></section><section class="panel"><div class="panel-head"><h2>Agent 性格</h2><p class="subheading">保存后从下一轮对话开始生效，重启后保留。</p></div>
      <form id="persona-form" class="settings-form">${fields.map(([key, label, max, hint]) => `<label class="field" for="persona-${key}">${label}${key === 'name' ? `<input id="persona-${key}" name="${key}" maxlength="${max}" required value="${esc(data.persona[key])}">` : `<textarea id="persona-${key}" name="${key}" maxlength="${max}">${esc(data.persona[key])}</textarea>`}<span class="hint">${hint}</span></label>`).join('')}
      <small>各项合计最多 6000 字，实际可用长度受上下文容量限制。</small><div id="persona-error" class="error-inline" role="alert"></div><div class="settings-actions"><button type="button" class="btn" id="persona-defaults">填入配置文件默认值</button><button type="submit" class="btn primary">保存性格${icon('arrow')}</button></div></form></section>
      <section class="panel" id="persona-derivation"><div class="panel-head"><h2>人格衍生</h2><p class="subheading">由原始人格推导，独立存储并可随时调整、回退。</p></div><div class="derivation-content settings-form"></div></section>
      <section class="panel reset-panel"><div class="panel-head"><h2>重置 Agent</h2><p class="subheading">清空所有对话、长期记忆、修订历史、后台任务和 QQ 个人会话，从头开始。</p></div><div class="settings-form"><p class="subheading">服务连接配置、API Token 和 WebUI 密码会保留。默认保留当前性格，也可以在确认时恢复配置文件中的默认性格。此操作无法撤销，不会删除已有备份。</p>${data.graph_cleanup_pending ? '<div class="notice warn">图谱清理正在等待后台处理；旧记忆已停止参与召回，图谱服务恢复后会继续清理。</div>' : ''}<div class="settings-actions"><button class="btn danger" id="reset-agent">${icon('refresh')}重置 Agent</button></div></div></section></div><aside class="agent-aside">${qqCard(qq)}<section class="panel context-card"><div class="panel-head"><h2>上下文与记忆</h2></div><div class="context-facts"><div><small>上下文预算</small><strong>${fmt(conversation.context.context_tokens)} tokens</strong></div><div><small>下一轮上下文（估算）</small><strong>${fmt(conversation.context.context_used_tokens)} tokens</strong></div><div><small>定期提取记忆</small><strong>每 ${fmt(conversation.context.memory_extract_tokens)} tokens</strong></div><div><small>已压缩历史</small><strong>${fmt(conversation.context.compactions)} 次</strong></div></div><p class="hint">QQ、网页和 API 共用上下文。占用包含系统指令、历史、摘要、记忆和工具，按 Token 预算压缩历史。API 实际用量与本地估算、长期记忆提取独立统计，可在聊天底部查看。</p></section></aside></div></div>`;
    collapsibleSettings($('#persona-form').closest('.panel'), 'persona');
    collapsibleSettings($('.reset-panel'), 'reset');
    collapsibleSettings($('.agent-aside .job-card'), 'qq');
    collapsibleSettings($('.context-card'), 'context');
    const refreshDerivation = await derivationPanel(epoch); if (epoch !== state.epoch) return;
    await stickerPanel(epoch); if (epoch !== state.epoch) return;
    const personaForm = $('#persona-form');
    $('#persona-defaults').onclick = () => { for (const [key] of fields) personaForm.elements[key].value = data.default_persona[key]; toast('已填入默认性格，点击“保存性格”后生效。'); };
    personaForm.onsubmit = async event => {
      event.preventDefault(); const button = $('button[type=submit]', personaForm); const errorEl = $('#persona-error'); button.disabled = true; errorEl.textContent = '';
      const persona = Object.fromEntries(fields.map(([key]) => [key, personaForm.elements[key].value]));
      try { await send('/v1/console/persona', persona, 'PUT'); if (epoch === state.epoch) { $('#agent-name').textContent = persona.name; await refreshDerivation(); toast('原始人格已保存，画像状态已更新。'); } }
      catch (error) { if (error.name !== 'AbortError') errorEl.textContent = errorText(error); }
      finally { button.disabled = false; }
    };
    $('#reset-agent').onclick = () => {
      const dialog = openDialog('confirm', `<form id="reset-form"><header class="dialog-head"><div><div class="eyebrow">RESET AGENT</div><h2>确认重新开始？</h2></div><button type="button" class="icon-btn" data-close="confirm" aria-label="取消重置">${icon('close')}</button></header><div class="dialog-body stack"><p class="subheading">所有对话、长期记忆及其来源和修订记录、QQ 个人会话与待发送回复都将被清空。此操作无法撤销，已有备份不受影响。</p><label class="check-label"><input type="checkbox" name="reset_persona">同时恢复配置文件中的默认性格</label><label class="field">输入 RESET 确认<input name="confirmation" autocomplete="off" spellcheck="false" pattern="RESET" required placeholder="RESET"></label><div class="error-inline" id="reset-error" role="alert"></div></div><footer class="dialog-footer"><button type="button" class="btn" data-close="confirm">取消</button><button type="submit" class="btn danger">确认重置</button></footer></form>`);
      $('#reset-form', dialog).onsubmit = async event => {
        event.preventDefault(); const form = event.target; const button = $('button[type=submit]', form); button.disabled = true; $('#reset-error').textContent = '';
        try { await send('/v1/admin/reset', { confirmation: form.elements.confirmation.value, reset_persona: form.elements.reset_persona.checked }); closeDialog('confirm'); state.detail = null; state.library = null; toast('Agent 已重置，对话与记忆已清空，图谱将在后台清理。'); route(); }
        catch (error) { if (error.name !== 'AbortError' && state.authenticated) $('#reset-error', dialog).textContent = errorText(error); button.disabled = false; }
      };
    };
  }
  async function settingsPage(epoch) {
    const data = await api('/v1/console/settings'); if (epoch !== state.epoch) return;
    $('#page').innerHTML = `<div class="settings-stack security-settings">      <section class="panel"><div class="panel-head between"><div><h2>WebUI 登录密码</h2><p class="subheading">控制台仅使用密码登录，刷新页面或重启服务后保持登录。</p></div><span class="badge ${data.password_configured ? 'green' : ''}">${data.password_configured ? '已设置' : '未设置'}</span></div>
      <form id="password-form" class="settings-form">${data.password_configured ? '<label class="field">当前密码<input name="current_password" type="password" autocomplete="current-password" maxlength="128" required></label>' : ''}
      <label class="field">新密码<input name="new_password" type="password" autocomplete="new-password" minlength="10" maxlength="128" required><span class="hint">10–128 个字符，支持中文和空格。</span></label><label class="field">确认新密码<input name="confirm_password" type="password" autocomplete="new-password" minlength="10" maxlength="128" required></label>
      <small>修改密码后，所有已登录设备立即退出。登录有效期为 ${fmt(data.session_days)} 天，使用时自动续期。</small><div id="password-error" class="error-inline" role="alert"></div><div class="settings-actions"><button type="submit" class="btn primary">保存密码${icon('shield')}</button></div></form></section>
</div>`;
    const passwordForm = $('#password-form');
    const savePassword = async newPassword => {
      const current = passwordForm.elements.current_password?.value || '';
      await send('/v1/console/password', { current_password: current, new_password: newPassword }, 'PUT');
      await logout(); toast('密码已保存，请重新登录。');
    };
    passwordForm.onsubmit = async event => {
      event.preventDefault(); const button = $('button[type=submit]', passwordForm); const errorEl = $('#password-error'); errorEl.textContent = '';
      if (passwordForm.elements.new_password.value !== passwordForm.elements.confirm_password.value) { errorEl.textContent = '两次输入的新密码不一致。'; return; }
      button.disabled = true;
      try { await savePassword(passwordForm.elements.new_password.value); }
      catch (error) { if (error.name !== 'AbortError') errorEl.textContent = errorText(error); button.disabled = false; }
    };
  }
  function uptime(seconds) { const hours = Math.floor(seconds / 3600); return hours >= 24 ? `${Math.floor(hours / 24)} 天 ${hours % 24} 时` : hours ? `${hours} 时 ${Math.floor(seconds % 3600 / 60)} 分` : `${Math.floor(seconds / 60)} 分`; }
  function bytes(n) { return n >= 1024 ** 3 ? `${(n / 1024 ** 3).toFixed(2)} GB` : n >= 1024 ** 2 ? `${(n / 1024 ** 2).toFixed(1)} MB` : `${(n / 1024).toFixed(0)} KB`; }
  document.addEventListener('click', event => {
    const target = event.target.closest('button,[data-detail]'); if (!target || target.disabled) return;
    if (target.dataset.image) { target.disabled = true; showImage(target.dataset.image, target.parentElement).catch(error => { toast(errorText(error), true); target.disabled = false; }); }
    else if (target.hasAttribute('data-new')) edit();
    else if (target.dataset.detail) detail(target.dataset.detail);
    else if (target.dataset.edit) edit(target.dataset.edit);
    else if (target.dataset.delete) remove(target.dataset.delete);
    else if (target.dataset.close) closeDialog(target.dataset.close);
    else if (target.hasAttribute('data-reload')) route();
    else if (target.hasAttribute('data-page')) { state.filter.offset = Number(target.dataset.page); loadLibrary(); }
    else if (target.hasAttribute('data-queue-view')) selectLibraryView('queue');
    else if (target.dataset.status) { state.filter.status = target.dataset.status; state.filter.offset = 0; selectLibraryView('memories'); }
    else if (target.dataset.example) { $('#retrieval-query').value = target.dataset.example; $('#retrieval-query').focus(); }
    else if (target.dataset.maintenance) {
      const epoch = state.epoch; target.disabled = true; target.dataset.busy = 'true';
      send(`/v1/admin/${target.dataset.maintenance}`, {}).then(async data => {
        toast(data.retried !== undefined ? `已重新排队 ${data.retried} 个任务。` : `已安排 ${data.queued} 条记忆同步索引。`);
        if (epoch === state.epoch && state.route === 'memories') await refreshQueue(epoch);
      }).catch(error => { if (error.name !== 'AbortError') toast(errorText(error), true); }).finally(() => {
        delete target.dataset.busy;
        target.disabled = target.dataset.maintenance === 'jobs/retry' && Number($('#queue-dead')?.textContent.replaceAll(',', '')) === 0;
      });
    }
  });
  for (const dialog of $$('dialog')) { dialog.addEventListener('close', () => { if (!$$('dialog[open]').length) document.body.classList.remove('dialog-open'); }); dialog.addEventListener('click', event => { if (event.target === dialog) { const r = dialog.getBoundingClientRect(); if (event.clientX < r.left || event.clientX > r.right || event.clientY < r.top || event.clientY > r.bottom) closeDialog(dialog.id); } }); }
  window.addEventListener('hashchange', () => { setNavigation(false); for (const dialog of $$('dialog')) if (dialog.open) closeDialog(dialog.id); state.detail = null; route(); });
  document.addEventListener('keydown', event => { if (event.key === 'Escape') setNavigation(false); if (event.key === '/' && state.authenticated && !['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement.tagName) && !$$('dialog[open]').length) { event.preventDefault(); focusSearch(); } });
  async function restoreLogin() {
    $('#app').innerHTML = loading('正在恢复登录…');
    try { await api('/v1/console/auth'); state.authenticated = true; shell(); await route(); }
    catch (error) {
      if (error.status === 401) login();
      else {
        $('#app').innerHTML = `<main class="login-form-wrap">${empty('暂时无法连接控制台', '登录状态已保留，请检查服务连接后重试。', '<button class="btn" id="retry-login">重新连接</button>')}</main>`;
        $('#retry-login').onclick = restoreLogin;
      }
    }
  }
  restoreLogin();
})();
