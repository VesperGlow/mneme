'use strict';
(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
  const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
  const kinds = { fact: '事实', preference: '偏好', event: '经历', relationship: '关系', emotion: '情绪' };
  const colors = { fact: '#73906c', preference: '#c7ac7d', event: '#9aaec3', relationship: '#b4a0bb', emotion: '#c49b92' };
  const pages = { chat: ['对话', 'CHAT', '把想法，慢慢说给我听', '让 Mneme 自主搜索、阅读网页，或一起看一张图片。'], tools: ['工具记录', 'TOOLS', '每一次行动，都看得见', '查看浏览器会话、工具调用、来源、耗时与错误。'], overview: ['概览', 'OVERVIEW', '让记忆，有迹可循。', '留意每一段积累，也看见记忆的变化。'], memories: ['记忆库', 'MEMORY LIBRARY', '整理你的记忆空间', '浏览、追溯与更新，让重要的事始终清晰。'], retrieval: ['检索实验室', 'RETRIEVAL LAB', '看见一次回想的过程', '从候选召回到精排，理解 Mneme 如何找到记忆。'], system: ['系统状态', 'SYSTEM STATUS', '一切运行，尽在眼前', '查看服务状态、调用表现与后台处理进度。'], settings: ['Agent 设置', 'AGENT SETTINGS', '让陪伴，更合你的心意', '调整性格、管理登录密码，或重新开始一段陪伴。'] };
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
  const state = { chatBusy: false, authenticated: false, route: 'overview', epoch: 0, controller: new AbortController(), filter: { q: '', kind: '', status: 'all', sort: 'updated', offset: 0 }, detail: null, library: null, poll: 0 };
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
  const heading = (action = '') => { const [, eyebrow, title, description] = pages[state.route]; return `<header class="page-heading"><div><div class="eyebrow">${eyebrow}</div><h1${state.route === 'overview' ? ' class="serif"' : ''}>${title}</h1><p class="subheading">${description}</p></div>${action}</header>`; };
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
    state.chatBusy = false;
    state.authenticated = false; state.controller.abort(); state.controller = new AbortController(); state.epoch++; state.detail = null; state.library = null;
    state.filter = { q: '', kind: '', status: 'all', sort: 'updated', offset: 0 }; clearTimeout(state.poll);
    for (const modal of $$('dialog')) { if (modal.open) modal.close(); modal.innerHTML = ''; }
    $('#toast').textContent = ''; $('#toast').className = ''; document.body.classList.remove('dialog-open'); login();
  }
  function shell() {
    $('#app').innerHTML = `<aside class="sidebar" aria-label="主导航"><a class="brand" href="#overview"><img src="/console/icon.svg" alt=""><div class="brand-text"><div class="brand-name">mneme</div><div class="brand-caption">CONSOLE</div></div></a><div class="nav-label">WORKSPACE</div><nav class="nav">${Object.entries(pages).map(([key, [name]], i) => `<a href="#${key}" data-nav="${key}" aria-label="${name}">${icon({ chat: 'spark', tools: 'chip', overview: 'grid', memories: 'book', retrieval: 'search', system: 'system', settings: 'edit' }[key])}<span>${key === 'settings' ? '设置' : key === 'retrieval' ? '检索' : name}</span></a>`).join('')}</nav><div class="sidebar-foot"><div class="flex">${icon('shield')}私有记忆空间</div><p>让每一段陪伴，都有记忆。<br>MNEME · LOCAL CONSOLE</p></div></aside><div class="workspace"><header class="topbar"><div class="breadcrumb">你的记忆工作台<span>/</span><strong id="breadcrumb-current">概览</strong></div><div class="top-actions"><button class="search-shortcut" id="search-shortcut">${icon('search')}搜索记忆<kbd>/</kbd></button><div class="verified"><i class="dot"></i>身份已验证</div><i class="top-divider"></i><button id="refresh" class="icon-btn" aria-label="刷新当前页面" title="刷新">${icon('refresh')}</button><button id="logout" class="icon-btn" aria-label="退出登录" title="退出登录">${icon('logout')}</button></div></header><main class="page"><div id="page" aria-live="polite"></div><footer class="page-foot"><span>Mneme Console · 让记忆成为陪伴的延续</span><span>PRIVATE WORKSPACE</span></footer></main></div>`;
    $('#logout').onclick = logout; $('#refresh').onclick = route; $('#search-shortcut').onclick = focusSearch;
  }
  async function route() {
    if (!state.authenticated) return;
    for (const url of objectURLs) URL.revokeObjectURL(url); objectURLs.clear();
    clearTimeout(state.poll); const epoch = ++state.epoch;
    state.route = Object.hasOwn(pages, location.hash.slice(1)) ? location.hash.slice(1) : 'overview';
    $$('[data-nav]').forEach(link => { const active = link.dataset.nav === state.route; link.classList.toggle('active', active); if (active) link.setAttribute('aria-current', 'page'); else link.removeAttribute('aria-current'); });
    $('#breadcrumb-current').textContent = pages[state.route][0]; document.title = `${pages[state.route][0]} · Mneme Console`;
    $('#page').innerHTML = heading(['overview', 'memories'].includes(state.route) ? newButton() : '') + loading(state.route === 'system' ? '正在探测服务状态…' : '正在读取数据…');
    try { await ({ overview, memories: library, retrieval: retrievalPage, system: systemPage, settings: settingsPage, chat: chatPage, tools: toolsPage }[state.route])(epoch); }
    catch (error) { if (epoch === state.epoch && error.name !== 'AbortError') $('#page').innerHTML = heading() + `<section class="panel">${empty('暂时无法读取数据', errorText(error), '<button class="btn" data-reload>重新加载</button>')}</section>`; }
  }
  function focusSearch() { if (!state.authenticated) return; if (state.route === 'memories') $('#memory-search')?.focus(); else { location.hash = 'memories'; setTimeout(() => $('#memory-search')?.focus(), 150); } }
  async function overview(epoch) {
    const [stats, latest] = await Promise.all([api('/v1/console/stats'), api('/v1/memories?limit=5&sort=created')]); if (epoch !== state.epoch) return;
    const pending = Number(stats.jobs.pending || 0) + Number(stats.jobs.running || 0);
    const cards = [['记忆总量', stats.total, '所有未删除的记忆', 'book'], ['有效记忆', stats.active, `${fmt(stats.invalid)} 条记忆已失效`, 'leaf'], ['近期新增', stats.recent_7_days, '过去 7 × 24 小时', 'spark'], ['后台任务', pending, stats.jobs.dead ? `${fmt(stats.jobs.dead)} 个任务需要重试` : '提取与索引更新', 'clock']];
    $('#page').innerHTML = heading(newButton()) + `<section class="metrics" aria-label="记忆统计">${cards.map(([label, n, hint, glyph], i) => `<article class="metric${i === 1 ? ' featured' : ''}"><div class="metric-label">${label}${icon(glyph)}</div><div class="metric-number">${fmt(n)}<small>${i === 3 ? '个' : '条'}</small></div><div class="metric-hint">${esc(hint)}</div></article>`).join('')}</section><section class="charts"><article class="panel"><div class="panel-head between"><div><h2>记忆的生长</h2><p class="subheading">最近 7 个自然日的新增记忆 · UTC</p></div><span class="badge">近 7 日</span></div><div class="chart-wrap">${activityChart(stats.activity)}</div></article><article class="panel"><div class="panel-head"><h2>每种记忆，都有位置</h2><p class="subheading">按记忆类型分布</p></div>${distribution(stats.kinds, stats.total)}</article></section><section class="panel"><div class="panel-head between"><div><h2>最近留下的片段</h2><p class="subheading">来自交流，也来自你的用心整理</p></div><a class="btn ghost small" href="#memories">全部记忆${icon('arrow')}</a></div>${latest.memories.length ? `<div class="table-scroll"><table class="recent-table"><thead><tr><th>记忆内容</th><th>类型</th><th>创建时间</th></tr></thead><tbody>${latest.memories.map(m => `<tr><td class="memory-cell"><button class="text-btn memory-title" data-detail="${esc(m.id)}">${esc(m.content)}</button><div class="memory-meta">${esc(entityArray(m).slice(0, 3).map(e => `# ${e}`).join('   ')) || (m.origin === 'manual' ? '手动记录' : '来自对话')}</div></td><td>${kindBadge(m.kind)}</td><td class="table-time">${date(m.created_at, true)}</td></tr>`).join('')}</tbody></table></div>` : empty('记忆，从第一段交流开始', '与 Mneme 交流后，值得留下的信息会逐渐积累。你也可以手动记录一条记忆。', newButton())}</section>`;
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
    const f = state.filter;
    $('#page').innerHTML = heading(newButton()) + `<div class="tabs" role="group" aria-label="记忆状态">${[['all', '全部记忆'], ['active', '有效'], ['invalid', '已失效'], ['deleted', '已删除']].map(([key, label]) => `<button class="tab${f.status === key ? ' active' : ''}" data-status="${key}" aria-pressed="${f.status === key}">${label}</button>`).join('')}</div><section class="panel"><form class="filters" id="filter-form"><div class="search-field">${icon('search')}<label class="sr-only" for="memory-search">搜索记忆</label><input id="memory-search" type="search" value="${esc(f.q)}" placeholder="搜索内容、实体或属性键…" maxlength="500" autocomplete="off"></div><label class="sr-only" for="kind-filter">筛选记忆类型</label><select id="kind-filter" class="filter-select"><option value="">所有类型</option>${Object.entries(kinds).map(([key, name]) => `<option value="${key}"${f.kind === key ? ' selected' : ''}>${name}</option>`).join('')}</select><label class="sr-only" for="sort-filter">排序方式</label><select id="sort-filter" class="filter-select">${[['updated', '最近更新'], ['created', '最近创建'], ['oldest', '最早创建'], ['importance', '重要性优先'], ['confidence', '可信度优先']].map(([key, name]) => `<option value="${key}"${f.sort === key ? ' selected' : ''}>${name}</option>`).join('')}</select></form><div id="library-results">${loading()}</div></section>`;
    $('#filter-form').onsubmit = event => { event.preventDefault(); clearTimeout(library.debounce); state.filter.q = $('#memory-search').value; state.filter.offset = 0; loadLibrary(epoch); };
    $('#memory-search').oninput = () => { clearTimeout(library.debounce); library.debounce = setTimeout(() => { if (epoch !== state.epoch) return; state.filter.q = $('#memory-search').value; state.filter.offset = 0; loadLibrary(epoch); }, 280); };
    $('#kind-filter').onchange = event => { state.filter.kind = event.target.value; state.filter.offset = 0; loadLibrary(epoch); };
    $('#sort-filter').onchange = event => { state.filter.sort = event.target.value; state.filter.offset = 0; loadLibrary(epoch); };
    await loadLibrary(epoch);
  }
  async function loadLibrary(epoch = state.epoch) {
    const sequence = library.sequence = (library.sequence || 0) + 1; const element = $('#library-results'); if (!element) return;
    element.setAttribute('aria-busy', 'true');
    try {
      const query = new URLSearchParams({ ...state.filter, limit: 15 }); const data = await api(`/v1/memories?${query}`);
      if (epoch !== state.epoch || sequence !== library.sequence) return;
      state.library = data; element.removeAttribute('aria-busy');
      element.innerHTML = data.memories.length ? `<div class="table-scroll"><table class="library-table"><thead><tr><th>记忆内容</th><th>类型</th><th>状态</th><th>重要性</th><th>可信度</th><th>更新时间</th><th><span class="sr-only">操作</span></th></tr></thead><tbody>${data.memories.map(m => `<tr><td class="memory-cell"><button class="text-btn memory-title" data-detail="${esc(m.id)}">${esc(m.content)}</button><div class="memory-meta">${esc(entityArray(m).slice(0, 3).map(e => `# ${e}`).join('   ')) || (m.origin === 'manual' ? '手动记录' : '来自对话')}${m.indexed_revision < m.revision ? ' · 索引更新中' : ''}</div></td><td class="cell-kind">${kindBadge(m.kind)}</td><td class="cell-state">${statusBadge(m)}</td><td class="cell-score" data-label="重要性">${signal(m.importance)}</td><td class="cell-score" data-label="可信度">${signal(m.confidence)}</td><td class="cell-time table-time">${date(m.updated_at, true)}</td><td class="cell-actions"><div class="actions"><button class="icon-btn table-action" data-edit="${esc(m.id)}" aria-label="编辑记忆"${m.deleted_at || m.status !== 'active' ? ' disabled' : ''}>${icon('edit')}</button><button class="icon-btn table-action danger" data-delete="${esc(m.id)}" aria-label="删除记忆"${m.deleted_at ? ' disabled' : ''}>${icon('trash')}</button></div></td></tr>`).join('')}</tbody></table></div>${pagination(data)}` : empty(state.filter.q || state.filter.kind || state.filter.status !== 'all' ? '没有找到匹配的记忆' : '这里还没有记忆', state.filter.status === 'deleted' ? '已删除的记忆不会参与召回。' : '试试其他关键词或筛选条件，或者手动新增一条记忆。', state.filter.status === 'all' && !state.filter.q ? newButton() : '') + pagination(data);
      if (data.memories.some(m => m.indexed_revision < m.revision)) { clearTimeout(state.poll); state.poll = setTimeout(() => { if (epoch === state.epoch && !$('#editor').open && !$('#confirm').open) loadLibrary(epoch); }, 5000); }
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
    $('#page').innerHTML = heading() + `<section class="panel"><form class="lab-query" id="retrieval-form"><label for="retrieval-query" class="sr-only">检索问题</label><textarea id="retrieval-query" maxlength="4000" placeholder="输入一个真实的问题，看看 Mneme 会想起什么…" required></textarea><div class="lab-bottom"><div class="examples"><button type="button" class="example" data-example="我平时喜欢喝什么咖啡？">咖啡偏好</button><button type="button" class="example" data-example="我现在住在哪座城市？">目前住址</button></div><button class="btn primary" type="submit" id="run-retrieval">运行检索${icon('arrow')}</button></div></form></section><div id="retrieval-results">${empty('从一个问题，开始回想', '这里会展示真实的召回候选、精排分数、筛除原因与最终结果。')}</div>`;
    $('#retrieval-form').onsubmit = async event => { event.preventDefault(); const query = $('#retrieval-query').value.trim(); if (!query) return; const epoch = state.epoch; const button = $('#run-retrieval'); button.disabled = true; $('#retrieval-results').innerHTML = loading('正在检索与精排…'); try { const data = await send('/v1/console/retrieval', { query }); if (epoch !== state.epoch) return; renderRetrieval(data); } catch (error) { if (epoch === state.epoch && error.name !== 'AbortError') $('#retrieval-results').innerHTML = empty('这次回想没有完成', errorText(error)); } finally { if (epoch === state.epoch) button.disabled = false; } };
  }
  function renderRetrieval(data) {
    const trace = data.trace; const labels = { vector: ['向量召回', 'Neo4j HNSW · 索引分数'], keyword: ['关键词召回', '中文 CJK 全文 · Lucene 分数'], graph_entity: ['实体匹配', '查询中直接提及的实体'], graph_neighbor: ['图谱扩展', '共享实体的两跳邻居'], sqlite_vector: ['近期向量补充', 'SQLite · cosine 相似度'], sqlite_keyword: ['近期关键词补充', 'SQLite · 中文双字重叠'] };
    const reason = { inactive: '已失效', changed_during_search: '检索中发生修改', below_rerank_threshold: '低于精排阈值', context_limit: '超出条数或上下文预算' };
    $('#retrieval-results').innerHTML = `<section class="panel trace-summary"><strong>${data.degraded ? '部分通道不可用，已使用可用结果' : '本次检索已完成'}</strong><span class="badge ${data.degraded ? 'amber' : 'green'}"><i class="dot"></i>${data.degraded ? '降级检索' : '完整检索'}</span><span class="badge">总耗时 ${ms(trace.total_ms)}</span><span class="badge">编码 ${ms(trace.embedding_ms)}</span><span class="badge">精排 ${ms(trace.rerank_ms)}</span></section><section class="panel spaced"><div class="panel-head between"><div><h2>最终想起的记忆</h2><p class="subheading">精排、时效与上下文预算共同决定</p></div><span class="badge green">${data.results.length} 条</span></div>${data.results.length ? `<div class="recall-list">${data.results.map((r, i) => `<article class="recall-row"><span class="hit-rank">${i + 1}</span><div class="recall-content"><button class="text-btn prewrap" data-detail="${esc(r.memory.id)}">${esc(r.memory.content)}</button><small>${r.channels.map(c => esc(labels[c]?.[0] || c)).join(' · ')}</small></div><span class="hit-score">${r.score.toFixed(4)}</span></article>`).join('')}</div>` : empty('没有足够相关的记忆', '可以调整问题，或先检查记忆是否已生效、索引是否已同步。')}</section><div class="spaced between"><div><div class="eyebrow">RECALL CHANNELS</div><h2>候选从哪里来</h2></div><small>不同通道的原始分数不可直接比较</small></div><section class="channel-grid spaced">${trace.channels.map(channel => `<article class="panel"><header class="panel-head between"><div><h2>${esc(labels[channel.name]?.[0] || channel.name)}</h2><p class="subheading">${esc(labels[channel.name]?.[1] || '')}</p></div><span class="badge ${channel.state === 'healthy' ? '' : 'amber'}">${channel.state === 'healthy' ? ms(channel.duration_ms) : '不可用'}</span></header><div class="channel-hits">${channel.hits.length ? channel.hits.map(hit => `<div class="hit"><span class="hit-rank">${hit.rank}</span><div class="hit-body">${hit.content ? `<button class="text-btn" data-detail="${esc(hit.id)}">${esc(hit.content)}</button>` : '<span class="muted">内容已删除或不可用</span>'}${hit.excluded ? '<span class="badge">不参与召回</span>' : ''}</div><span class="hit-score">${hit.score === null ? '—' : hit.score.toFixed(4)}</span></div>`).join('') : '<div class="empty">该通道没有候选</div>'}</div></article>`).join('')}</section><section class="panel spaced"><div class="panel-head"><h2>Reranker 与最终排序</h2><p class="subheading">${trace.reranker_state === 'healthy' ? `精排阈值 ${data.rerank_min_score} · 最终得分包含相关性、融合排名和时间强度` : trace.reranker_state === 'skipped' ? '没有有效候选，未调用精排' : '精排不可用，按 RRF 融合排名回退'}</p></div><div class="table-scroll"><table><thead><tr><th>候选记忆</th><th>精排顺序</th><th>RRF</th><th>Reranker</th><th>最终得分</th><th>结果</th></tr></thead><tbody>${trace.candidates.map(c => `<tr><td class="memory-cell">${esc(c.content || '内容已删除或不可用')}</td><td>${c.rerank_rank ?? '—'}</td><td>${c.rrf_score.toFixed(4)}</td><td>${c.rerank_score === null ? '—' : c.rerank_score.toFixed(4)}</td><td>${c.final_score === null ? '—' : c.final_score.toFixed(4)}</td><td><span class="badge${c.selected ? ' green' : ''}">${c.selected ? '已召回' : esc(reason[c.reason] || '未选中')}</span></td></tr>`).join('') || '<tr><td colspan="6" class="muted">没有有效候选</td></tr>'}</tbody></table></div></section>`;
  }
  async function systemPage(epoch) {
    const [data, qq] = await Promise.all([api('/v1/console/system'), api('/v1/qq/status')]); if (epoch !== state.epoch) return;
    const labels = { llm: ['语言模型', 'spark', '外部兼容 API'], neo4j: ['Neo4j', 'graph', '知识图谱与混合检索'], embedding: ['Embedding', 'chip', '本地 CPU 语义编码'], reranker: ['Reranker', 'system', '本地 CPU 相关性精排'] };
    const services = Object.entries(data.services).map(([key, s]) => { const [label, glyph, desc] = labels[key]; const metric = s.metrics; return `<article class="panel service"><div class="between"><div class="flex"><div class="service-icon">${icon(glyph)}</div><div><div class="service-title">${label}</div><div class="service-model">${desc}</div></div></div><span class="badge ${s.state === 'healthy' ? 'green' : s.state === 'unknown' ? 'amber' : 'red'}"><i class="dot"></i>${s.state === 'healthy' ? '可用' : s.state === 'unknown' ? '待确认' : '不可用'}</span></div><p class="service-model spaced">${esc(s.model || 'Neo4j · Cypher HTTP')}</p><div class="service-metrics"><div><small>探测耗时</small><strong>${s.latency_ms.toFixed(0)}<span>ms</span></strong></div><div><small>平均调用耗时</small><strong>${metric.requests ? ms(metric.average_ms) : '—'}</strong></div><div><small>调用 / 失败</small><strong>${fmt(metric.requests)}<span>/ ${fmt(metric.failures)}</span></strong></div></div></article>`; }).join('');
    $('#page').innerHTML = heading() + `<div class="notice">${icon('info')}<span>LLM 通过模型目录接口探测，不执行生成；接口不受支持时显示“待确认”。CPU 模型状态表示服务已加载。性能统计来自当前进程已完成的真实调用。</span></div><section class="service-grid spaced">${services}</section><section class="panel spaced"><div class="system-facts"><div><small>连续运行</small><strong>${uptime(data.uptime_seconds)}</strong></div><div><small>SQLite 数据文件</small><strong>${bytes(data.database_bytes)}</strong></div><div><small>向量维度</small><strong>${data.dimensions}</strong></div><div><small>服务版本</small><strong>v${esc(data.version)}</strong></div></div></section><section class="panel job-card spaced"><div class="between"><div><h2>后台处理队列</h2><p class="subheading">记忆提取和索引同步持续在后台进行</p></div><div class="flex"><span class="badge">待处理 ${fmt(data.jobs.pending)}</span><span class="badge green">运行中 ${fmt(data.jobs.running)}</span><span class="badge${data.jobs.dead ? ' amber' : ''}">失败 ${fmt(data.jobs.dead)}</span></div></div><div class="between spaced"><small>SQLite 正常 · 检查时间 ${date(data.checked_at, true)}${data.cached ? ' · 10 秒内缓存' : ''}</small><div class="flex"><button class="btn small" data-maintenance="reindex">重建索引</button><button class="btn small" data-maintenance="jobs/retry"${data.jobs.dead ? '' : ' disabled'}>重试失败任务</button></div></div></section>`;
    $('#page').insertAdjacentHTML('beforeend', `<section class="panel job-card spaced"><div class="between"><div><h2>QQ 机器人</h2><p class="subheading">${qq.enabled ? `AppID ${esc(qq.app_id)} · 个人单聊` : '在 .env 填写 QQ AppID 和 AppSecret 后启用'}</p></div><span class="badge ${qq.has_session ? 'green' : 'amber'}">${qq.has_session ? '个人会话已建立' : qq.bound ? '等待新会话' : qq.enabled ? '等待首条私聊' : '未启用'}</span></div>${qq.enabled ? `<p class="hint spaced">直接私聊即可开始，无需绑定码。首次私聊自动建立个人会话，重启后继续使用。</p><div class="spaced"><small>待处理 ${fmt(qq.queue.pending)} · 运行中 ${fmt(qq.queue.running)} · 已发送 ${fmt(qq.queue.sent)} · 失败 ${fmt(qq.queue.dead)}</small></div>` : ''}</section>`);
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
  const contextUsage = context => `上下文 ${fmt(context.history_turns)} / ${fmt(context.max_history_turns)} 轮 · ${fmt(context.context_chars)} 字上限 · 已压缩 ${fmt(context.compactions)} 次 · 记忆提取 ${fmt(context.memory_pending_chars)} / ${fmt(context.memory_extract_chars)} 字`;
  function transcriptMarkup(history) {
    return history.messages.map(message => `<article class="chat-message ${message.role}" data-message-id="${esc(message.id)}"><small>${message.role === 'user' ? '你' : 'Mneme'} · ${message.channel === 'qq' ? 'QQ' : '网页/API'} · ${date(message.created_at, true)}${message.status === 'failed' ? ' · 本轮未完成' : message.status === 'generating' ? ' · 正在回复' : ''}</small><div class="prewrap">${esc(message.content)}</div><div class="chat-assets">${history.assets.filter(a => a.message_id === message.id).map(a => `<button class="btn small" data-image="${esc(a.id)}">查看上传图片</button>`).join('')}</div>${message.role === 'assistant' ? `<div class="chat-tools">${history.tool_calls.filter(c => c.turn_id === message.turn_id).reverse().map(toolMarkup).join('')}</div>` : ''}</article>`).join('') || empty('开始一段交流', '这里会显示 QQ 和网页中的聊天记录，也可以直接发送消息继续交流。');
  }
  async function chatPage(epoch) {
    const [tools, history] = await Promise.all([api('/v1/console/tools'), api('/v1/console/messages')]); if (epoch !== state.epoch) return;
    const data = history.conversation;
    const busy = state.chatBusy || Boolean(data.active_turn);
    if (busy) state.poll = setTimeout(() => { if (epoch === state.epoch) route(); }, 1500);
    $('#page').innerHTML = heading() + `<section class="panel chat-panel"><div class="chat-toolbar"><div class="conversation-status"><strong>持续对话</strong><small id="chat-context">${esc(contextUsage(data.context))}</small></div><span class="badge ${tools.state === 'healthy' ? 'green' : ''}">${tools.enabled ? tools.state === 'healthy' ? '浏览器工具可用' : '浏览器工具暂不可用' : '纯文本对话'}</span></div>${data.context.summary ? `<details class="conversation-summary"><summary>对话摘要 · 全局共享，达到上限自动整理</summary><p class="prewrap">${esc(data.context.summary)}</p></details>` : '<p class="conversation-hint">QQ、网页和 API 共用上下文，达到上限自动压缩，累计新增文字后定期提取长期记忆。</p>'}${data.active_turn ? '<p class="conversation-hint" role="status">上一轮回复正在继续生成，完成后会自动显示。</p>' : ''}<div id="chat-messages" class="chat-messages">${transcriptMarkup(history)}</div><form id="chat-form" class="chat-composer"><label class="sr-only" for="chat-message">消息</label><textarea id="chat-message" maxlength="4000" placeholder="和 Mneme 聊聊，或请它搜索、阅读网页…"${busy ? ' disabled' : ''}></textarea><div id="chat-attachments" class="chat-attachments"></div><div class="between"><label class="btn small image-upload">${icon('plus')}上传图片<input id="chat-files" type="file" accept="image/png,image/jpeg,image/webp" multiple${!tools.enabled || busy ? ' disabled' : ''}></label><small>最多 4 张 · PNG / JPEG / WebP · 每张 8 MiB · 需支持图片与工具调用的模型</small><button class="btn primary" id="send-chat"${busy ? ' disabled' : ''}>发送${icon('arrow')}</button></div><div id="chat-error" class="error-inline" role="alert"></div></form></section>`;
    const uploads = []; let uploading = false;
    const renderUploads = () => { $('#chat-attachments').innerHTML = uploads.map((a, i) => `<span class="tag">图片 ${i + 1}<button type="button" class="icon-btn" data-remove-upload="${i}" aria-label="移除图片 ${i + 1}">${icon('close')}</button></span>`).join(''); $$('[data-remove-upload]').forEach(button => { button.onclick = () => { uploads.splice(Number(button.dataset.removeUpload), 1); renderUploads(); }; }); };
    $('#chat-files').onchange = async event => {
      const files = [...event.target.files]; event.target.value = ''; const errorEl = $('#chat-error'); errorEl.textContent = '';
      if (files.length + uploads.length > 4 || files.some(file => file.size > 8 * 1024 * 1024)) { errorEl.textContent = '最多上传 4 张图片，每张不超过 8 MiB。'; return; }
      const sendButton = $('#send-chat'); sendButton.disabled = true; uploading = true;
      try { for (const file of files) { const asset = await api('/v1/images', { method: 'POST', body: file, headers: { 'Content-Type': 'application/octet-stream' } }); if (epoch !== state.epoch) return; uploads.push(asset.asset_id); renderUploads(); } }
      catch (error) { if (epoch === state.epoch && error.name !== 'AbortError') errorEl.textContent = '图片上传未完成，请检查图片格式与工具服务。'; }
      finally { uploading = false; if (epoch === state.epoch) sendButton.disabled = Boolean(data.active_turn); }
    };
    $('#chat-form').onsubmit = async event => {
      event.preventDefault(); if (state.chatBusy || data.active_turn) return;
      const message = $('#chat-message').value.trim() || (uploads.length ? '请查看我上传的图片。' : ''); if (!message) return;
      const assets = [...uploads]; const requestId = crypto.randomUUID(); const errorEl = $('#chat-error'); errorEl.textContent = ''; state.chatBusy = true;
      for (const element of $$('#chat-form button,#chat-form input,#chat-form textarea')) element.disabled = true;
      const messages = $('#chat-messages'); $('.empty', messages)?.remove();
      messages.insertAdjacentHTML('beforeend', `<article class="chat-message user"><small>你</small><div class="prewrap">${esc(message)}</div></article><article class="chat-message assistant" id="chat-current"><small>Mneme</small><div class="prewrap" id="chat-answer"></div><div class="chat-tools" id="chat-live-tools"></div><small id="chat-progress">正在思考…</small></article>`);
      let persisted = false; let failed = false;
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
            if (name === 'delta') { $('#chat-answer').textContent += data.content; $('#chat-progress').textContent = '正在回复…'; }
            if (name === 'tool') { const id = `tool-${data.id}`; let element = document.getElementById(id); if (!element) { element = document.createElement('div'); element.id = id; $('#chat-live-tools').append(element); } element.innerHTML = toolMarkup(data); $('#chat-progress').textContent = data.status === 'running' ? `正在${toolLabels[data.action] || '执行工具'}…` : '正在继续思考…'; }
            if (name === 'error') $('#chat-progress').textContent = '本轮失败，临时输出未保存。';
          }
          if (done) break;
        }
        if (!persisted) throw new Error(failed ? 'generation_failed' : 'stream_interrupted');
      } catch (error) { if (epoch === state.epoch && error.name !== 'AbortError') errorEl.textContent = `本轮未确认完成，可稍后刷新会话检查。${errorText(error)}`; }
      finally { state.chatBusy = false; if (epoch === state.epoch && state.authenticated) { if (persisted) route(); else { for (const element of $$('#chat-form button,#chat-form input,#chat-form textarea')) element.disabled = false; } } }
    };
    const last = $('.chat-message:last-child'); last?.scrollIntoView({ block: 'nearest' });
    if (!busy) {
      let previous = JSON.stringify([history.messages, history.assets, history.tool_calls]);
      const refreshTranscript = async () => {
        if (epoch !== state.epoch || state.chatBusy) return;
        try {
          if (!document.hidden) {
            const latest = await api('/v1/console/messages');
            if (epoch !== state.epoch || state.chatBusy) return;
            data.active_turn = latest.conversation.active_turn;
            $('#chat-context').textContent = contextUsage(latest.conversation.context);
            for (const element of $$('#chat-form button,#chat-form textarea')) element.disabled = uploading || Boolean(data.active_turn);
            $('#chat-files').disabled = uploading || !tools.enabled || Boolean(data.active_turn);
            const revision = JSON.stringify([latest.messages, latest.assets, latest.tool_calls]);
            if (revision !== previous) {
              const container = $('#chat-messages');
              const atBottom = container.scrollHeight - container.scrollTop - container.clientHeight < 60;
              container.innerHTML = transcriptMarkup(latest); previous = revision;
              if (atBottom) container.scrollTop = container.scrollHeight;
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
    $('#page').innerHTML = heading() + `<section class="panel job-card"><div class="between"><div><h2>mneme-tools</h2><p class="subheading">Playwright · Chromium / CDP · 真实浏览器搜索</p></div><span class="badge ${status.state === 'healthy' ? 'green' : 'amber'}">${status.enabled ? status.state === 'healthy' ? '可用' : '暂不可用' : '未启用'}</span></div><div class="system-facts"><div><small>浏览器会话</small><strong>${fmt(service.active_sessions)} / ${fmt(service.max_sessions)}</strong></div><div><small>每轮调用上限</small><strong>${fmt(status.max_calls)}</strong></div><div><small>调用 / 失败</small><strong>${fmt(status.metrics?.requests)} / ${fmt(status.metrics?.failures)}</strong></div><div><small>空闲回收</small><strong>${fmt((service.idle_timeout_seconds || 0) / 60)} 分</strong></div><div><small>浏览器沙箱</small><strong>${service.sandbox === true ? '已启用' : '不可用'}</strong></div><div><small>平均调用耗时</small><strong>${ms(status.metrics?.average_ms)}</strong></div></div><p class="hint">工具服务通过独立凭证与 Pod 内 localhost HTTP 连接。浏览器只允许访问公网 HTTP/HTTPS，图片由现有多模态 LLM 理解。</p></section><section class="panel spaced"><div class="panel-head"><h2>最近工具调用</h2><p class="subheading">状态、耗时、错误与实际读取的来源，最多显示 100 条</p></div><div class="tool-log-list">${data.calls.map(call => `<details class="tool-log"><summary><span>${esc(toolLabels[call.action] || call.name)}</span><span class="badge ${call.status === 'failed' ? 'red' : call.status === 'running' ? 'amber' : 'green'}">${esc(call.status)}</span><small>${ms(call.elapsed_ms)} · ${date(call.created_at, true)}</small></summary><div class="tool-log-body">${toolMarkup(call)}<p class="hint">会话 ${esc(call.session_id)} · 轮次 ${esc(call.turn_id)}</p><button class="btn small" data-close-browser="${esc(call.session_id)}">释放浏览器会话</button><details class="technical"><summary>结构化结果</summary><pre class="prewrap">${esc(JSON.stringify(call.result, null, 2))}</pre></details></div></details>`).join('') || empty('还没有工具调用', '前往对话页，让 Mneme 搜索网页、阅读资料或查看图片。', '<a class="btn" href="#chat">开始对话</a>')}</div></section>`;
    $$('[data-close-browser]').forEach(button => { button.onclick = async () => { button.disabled = true; try { await send('/v1/console/tools/close', { session_id: button.dataset.closeBrowser }); toast('浏览器会话已释放。'); route(); } catch (error) { toast(errorText(error), true); button.disabled = false; } }; });
    if (data.calls.some(call => call.status === 'running')) state.poll = setTimeout(() => { if (epoch === state.epoch) route(); }, 3000);
  }

  async function settingsPage(epoch) {
    const data = await api('/v1/console/settings'); if (epoch !== state.epoch) return;
    const fields = [['name', '名字', 80, '希望怎样称呼 Agent'], ['description', '性格描述', 6000, '身份、个性与陪伴方式'], ['style', '交流方式', 6000, '语气、表达习惯和回复偏好'], ['boundaries', '边界', 6000, '需要遵守的约定与能力边界'], ['output_format', '输出格式 Prompt', 6000, '例如：使用纯文本，分段回复，不使用 Markdown。留空表示不额外约束输出格式。']];
    $('#page').innerHTML = heading() + `<div class="settings-stack">
      <section class="panel"><div class="panel-head"><h2>Agent 性格</h2><p class="subheading">保存后从下一轮对话开始生效，重启后保留。</p></div>
      <form id="persona-form" class="settings-form">${fields.map(([key, label, max, hint]) => `<label class="field" for="persona-${key}">${label}${key === 'name' ? `<input id="persona-${key}" name="${key}" maxlength="${max}" required value="${esc(data.persona[key])}">` : `<textarea id="persona-${key}" name="${key}" maxlength="${max}">${esc(data.persona[key])}</textarea>`}<span class="hint">${hint}</span></label>`).join('')}
      <small>各项合计最多 6000 字，实际可用长度受上下文容量限制。</small><div id="persona-error" class="error-inline" role="alert"></div><div class="settings-actions"><button type="button" class="btn" id="persona-defaults">填入配置文件默认值</button><button type="submit" class="btn primary">保存性格${icon('arrow')}</button></div></form></section>
      <section class="panel"><div class="panel-head between"><div><h2>WebUI 登录密码</h2><p class="subheading">控制台仅使用密码登录，刷新页面或重启服务后保持登录。</p></div><span class="badge ${data.password_configured ? 'green' : ''}">${data.password_configured ? '已设置' : '未设置'}</span></div>
      <form id="password-form" class="settings-form">${data.password_configured ? '<label class="field">当前密码<input name="current_password" type="password" autocomplete="current-password" maxlength="128" required></label>' : ''}
      <label class="field">新密码<input name="new_password" type="password" autocomplete="new-password" minlength="10" maxlength="128" required><span class="hint">10–128 个字符，支持中文和空格。</span></label><label class="field">确认新密码<input name="confirm_password" type="password" autocomplete="new-password" minlength="10" maxlength="128" required></label>
      <small>修改密码后，所有已登录设备立即退出。登录有效期为 ${fmt(data.session_days)} 天，使用时自动续期。</small><div id="password-error" class="error-inline" role="alert"></div><div class="settings-actions"><button type="submit" class="btn primary">保存密码${icon('shield')}</button></div></form></section>
      <section class="panel reset-panel"><div class="panel-head"><h2>重置 Agent</h2><p class="subheading">清空所有对话、长期记忆、修订历史、后台任务和 QQ 个人会话，从头开始。</p></div><div class="settings-form"><p class="subheading">服务连接配置、API Token 和 WebUI 密码会保留。默认保留当前性格，也可以在确认时恢复配置文件中的默认性格。此操作无法撤销，不会删除已有备份。</p>${data.graph_cleanup_pending ? '<div class="notice warn">图谱清理正在等待后台处理；旧记忆已停止参与召回，图谱服务恢复后会继续清理。</div>' : ''}<div class="settings-actions"><button class="btn danger" id="reset-agent">${icon('refresh')}重置 Agent</button></div></div></section></div>`;
    const personaForm = $('#persona-form');
    $('#persona-defaults').onclick = () => { for (const [key] of fields) personaForm.elements[key].value = data.default_persona[key]; toast('已填入默认性格，点击“保存性格”后生效。'); };
    personaForm.onsubmit = async event => {
      event.preventDefault(); const button = $('button[type=submit]', personaForm); const errorEl = $('#persona-error'); button.disabled = true; errorEl.textContent = '';
      const persona = Object.fromEntries(fields.map(([key]) => [key, personaForm.elements[key].value]));
      try { await send('/v1/console/persona', persona, 'PUT'); if (epoch === state.epoch) toast('性格已保存，下一轮对话开始生效。'); }
      catch (error) { if (error.name !== 'AbortError') errorEl.textContent = errorText(error); }
      finally { button.disabled = false; }
    };
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
    $('#reset-agent').onclick = () => {
      const dialog = openDialog('confirm', `<form id="reset-form"><header class="dialog-head"><div><div class="eyebrow">RESET AGENT</div><h2>确认重新开始？</h2></div><button type="button" class="icon-btn" data-close="confirm" aria-label="取消重置">${icon('close')}</button></header><div class="dialog-body stack"><p class="subheading">所有对话、长期记忆及其来源和修订记录、QQ 个人会话与待发送回复都将被清空。此操作无法撤销，已有备份不受影响。</p><label class="check-label"><input type="checkbox" name="reset_persona">同时恢复配置文件中的默认性格</label><label class="field">输入 RESET 确认<input name="confirmation" autocomplete="off" spellcheck="false" pattern="RESET" required placeholder="RESET"></label><div class="error-inline" id="reset-error" role="alert"></div></div><footer class="dialog-footer"><button type="button" class="btn" data-close="confirm">取消</button><button type="submit" class="btn danger">确认重置</button></footer></form>`);
      $('#reset-form', dialog).onsubmit = async event => {
        event.preventDefault(); const form = event.target; const button = $('button[type=submit]', form); button.disabled = true; $('#reset-error').textContent = '';
        try { await send('/v1/admin/reset', { confirmation: form.elements.confirmation.value, reset_persona: form.elements.reset_persona.checked }); closeDialog('confirm'); state.detail = null; state.library = null; toast('Agent 已重置，对话与记忆已清空，图谱将在后台清理。'); route(); }
        catch (error) { if (error.name !== 'AbortError' && state.authenticated) $('#reset-error', dialog).textContent = errorText(error); button.disabled = false; }
      };
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
    else if (target.dataset.status) { state.filter.status = target.dataset.status; state.filter.offset = 0; $$('[data-status]').forEach(t => { t.classList.toggle('active', t === target); t.setAttribute('aria-pressed', String(t === target)); }); loadLibrary(); }
    else if (target.dataset.example) { $('#retrieval-query').value = target.dataset.example; $('#retrieval-query').focus(); }
    else if (target.dataset.maintenance) { target.disabled = true; send(`/v1/admin/${target.dataset.maintenance}`, {}).then(data => { toast(data.retried !== undefined ? `已重新排队 ${data.retried} 个任务。` : `已安排 ${data.queued} 条记忆同步索引。`); }).catch(error => { if (error.name !== 'AbortError') toast(errorText(error), true); }).finally(() => { target.disabled = false; }); }
  });
  for (const dialog of $$('dialog')) { dialog.addEventListener('close', () => { if (!$$('dialog[open]').length) document.body.classList.remove('dialog-open'); }); dialog.addEventListener('click', event => { if (event.target === dialog) { const r = dialog.getBoundingClientRect(); if (event.clientX < r.left || event.clientX > r.right || event.clientY < r.top || event.clientY > r.bottom) closeDialog(dialog.id); } }); }
  window.addEventListener('hashchange', () => { for (const dialog of $$('dialog')) if (dialog.open) closeDialog(dialog.id); state.detail = null; route(); });
  document.addEventListener('keydown', event => { if (event.key === '/' && state.authenticated && !['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement.tagName) && !$$('dialog[open]').length) { event.preventDefault(); focusSearch(); } });
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
