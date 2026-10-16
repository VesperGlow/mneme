'use strict';
(() => {
  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
  const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
  const kinds = { fact: '事实', preference: '偏好', event: '经历', relationship: '关系', emotion: '情绪' };
  const colors = { fact: '#73906c', preference: '#c7ac7d', event: '#9aaec3', relationship: '#b4a0bb', emotion: '#c49b92' };
  const pages = { overview: ['概览', 'OVERVIEW', '让记忆，有迹可循。', '留意每一段积累，也看见记忆的变化。'], memories: ['记忆库', 'MEMORY LIBRARY', '整理你的记忆空间', '浏览、追溯与更新，让重要的事始终清晰。'], retrieval: ['检索实验室', 'RETRIEVAL LAB', '看见一次回想的过程', '从候选召回到精排，理解 Mneme 如何找到记忆。'], system: ['系统状态', 'SYSTEM STATUS', '一切运行，尽在眼前', '查看服务状态、调用表现与后台处理进度。'] };
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
  const state = { token: '', route: 'overview', epoch: 0, controller: new AbortController(), filter: { q: '', kind: '', status: 'all', sort: 'updated', offset: 0 }, detail: null, lastActivity: Date.now(), library: null, poll: 0 };
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
  function lockIfIdle() {
    if (state.token && Date.now() - state.lastActivity > 30 * 60 * 1000) { logout(); toast('闲置已超过 30 分钟，控制台已锁定。'); return true; }
    return false;
  }
  async function api(path, options = {}) {
    if (lockIfIdle()) throw new ApiFailure(401, 'idle_timeout');
    const response = await fetch(path, { ...options, cache: 'no-store', credentials: 'omit', redirect: 'error', signal: state.controller.signal, headers: { Accept: 'application/json', ...(options.body ? { 'Content-Type': 'application/json' } : {}), Authorization: `Bearer ${state.token}`, ...options.headers } });
    const body = await response.json();
    if (!response.ok) {
      if (response.status === 401 && state.token && $('#page')) { logout(); toast('登录已失效，请重新输入 Token。', true); }
      throw new ApiFailure(response.status, body.error || 'request_failed');
    }
    return body;
  }
  const send = (path, data, method = 'POST') => api(path, { method, body: JSON.stringify(data) });
  function errorText(error) {
    if (error.code === 'duplicate_memory') return '已有相同内容的有效记忆，请编辑已有记录。';
    if (error.code === 'memory_changed_reload') return '这条记忆已发生变化，请关闭窗口后重新打开，核对最新版本。';
    if (error.status === 401) return 'Token 不正确，请重新输入。';
    if (error.status === 400 || error.status === 422) return '请检查输入：内容最多 500 字，实体最多 8 个，评分须在 0–100% 之间。';
    if (error.status === 404) return '没有找到这条记忆，请刷新列表。';
    return '暂时无法完成请求，请检查服务连接后重试。';
  }
  function empty(title, description, action = '') { return `<div class="empty"><div class="empty-icon">${icon('leaf')}</div><h3>${esc(title)}</h3><p>${esc(description)}</p>${action}</div>`; }
  const loading = text => `<div class="loading" role="status"><span class="spinner"></span>${esc(text || '正在读取记忆…')}</div>`;
  const heading = (action = '') => { const [, eyebrow, title, description] = pages[state.route]; return `<header class="page-heading"><div><div class="eyebrow">${eyebrow}</div><h1${state.route === 'overview' ? ' class="serif"' : ''}>${title}</h1><p class="subheading">${description}</p></div>${action}</header>`; };
  const newButton = () => `<button class="btn primary" data-new>${icon('plus')}新增记忆</button>`;
  function login() {
    $('#app').innerHTML = `<main class="login"><section class="login-story"><a class="brand" href="/console/" aria-label="Mneme Console"><img src="/console/icon.svg" alt=""><div class="brand-text"><div class="brand-name">mneme</div><div class="brand-caption">CONSOLE</div></div></a><div class="login-copy"><div class="eyebrow">A SPACE FOR WHAT MATTERS</div><h1 class="serif">每一段记忆，<br>都值得被好好保管。</h1><p>在这里，整理那些值得留下的片段，<br>让每一次回想，都有迹可循。</p></div><div class="login-footer">PRIVATE BY DEFAULT · BUILT WITH CARE</div><div class="login-decoration"></div></section><section class="login-form-wrap"><form id="login-form" class="login-card"><div class="eyebrow">YOUR PRIVATE CONSOLE</div><h2>进入你的记忆空间</h2><p class="subheading">使用 Mneme API Token 验证身份。</p><label class="field" for="token">API Token<input id="token" name="token" type="password" placeholder="输入你的访问凭证" autocomplete="off" spellcheck="false" minlength="32" maxlength="512" required autofocus></label><button class="btn primary" type="submit">解锁控制台${icon('arrow')}</button><div id="login-error" class="error-inline" role="alert"></div><div class="login-note">${icon('shield')}<span>凭证仅保存在当前页面内存中。刷新页面或闲置 30 分钟后，需重新验证身份。</span></div></form></section></main>`;
    $('#login-form').addEventListener('submit', async event => {
      event.preventDefault(); const button = $('button[type=submit]', event.target); button.disabled = true; $('#login-error').textContent = '';
      state.token = $('#token').value.trim(); $('#token').value = '';
      try { await api('/v1/console/auth'); state.lastActivity = Date.now(); shell(); route(); }
      catch (error) { state.token = ''; $('#login-error').textContent = errorText(error); button.disabled = false; $('#token').focus(); }
    });
  }
  function logout() {
    state.token = ''; state.controller.abort(); state.controller = new AbortController(); state.epoch++; state.detail = null; state.library = null;
    state.filter = { q: '', kind: '', status: 'all', sort: 'updated', offset: 0 }; clearTimeout(state.poll);
    for (const modal of $$('dialog')) { if (modal.open) modal.close(); modal.innerHTML = ''; }
    $('#toast').textContent = ''; $('#toast').className = ''; document.body.classList.remove('dialog-open'); login();
  }
  function shell() {
    $('#app').innerHTML = `<aside class="sidebar" aria-label="主导航"><a class="brand" href="#overview"><img src="/console/icon.svg" alt=""><div class="brand-text"><div class="brand-name">mneme</div><div class="brand-caption">CONSOLE</div></div></a><div class="nav-label">WORKSPACE</div><nav class="nav">${Object.entries(pages).map(([key, [name]], i) => `<a href="#${key}" data-nav="${key}" aria-label="${name}">${icon(['grid', 'book', 'search', 'system'][i])}<span>${name}</span></a>`).join('')}</nav><div class="sidebar-foot"><div class="flex">${icon('shield')}私有记忆空间</div><p>让每一段陪伴，都有记忆。<br>MNEME · LOCAL CONSOLE</p></div></aside><div class="workspace"><header class="topbar"><div class="breadcrumb">你的记忆工作台<span>/</span><strong id="breadcrumb-current">概览</strong></div><div class="top-actions"><button class="search-shortcut" id="search-shortcut">${icon('search')}搜索记忆<kbd>/</kbd></button><div class="verified"><i class="dot"></i>身份已验证</div><i class="top-divider"></i><button id="refresh" class="icon-btn" aria-label="刷新当前页面" title="刷新">${icon('refresh')}</button><button id="logout" class="icon-btn" aria-label="退出登录" title="退出登录">${icon('logout')}</button></div></header><main class="page"><div id="page" aria-live="polite"></div><footer class="page-foot"><span>Mneme Console · 让记忆成为陪伴的延续</span><span>PRIVATE WORKSPACE</span></footer></main></div>`;
    $('#logout').onclick = logout; $('#refresh').onclick = route; $('#search-shortcut').onclick = focusSearch;
  }
  async function route() {
    if (!state.token) return;
    clearTimeout(state.poll); const epoch = ++state.epoch;
    state.route = Object.hasOwn(pages, location.hash.slice(1)) ? location.hash.slice(1) : 'overview';
    $$('[data-nav]').forEach(link => { const active = link.dataset.nav === state.route; link.classList.toggle('active', active); if (active) link.setAttribute('aria-current', 'page'); else link.removeAttribute('aria-current'); });
    $('#breadcrumb-current').textContent = pages[state.route][0]; document.title = `${pages[state.route][0]} · Mneme Console`;
    $('#page').innerHTML = heading(['overview', 'memories'].includes(state.route) ? newButton() : '') + loading(state.route === 'system' ? '正在探测服务状态…' : '正在读取数据…');
    try { await ({ overview, memories: library, retrieval: retrievalPage, system: systemPage }[state.route])(epoch); }
    catch (error) { if (epoch === state.epoch && error.name !== 'AbortError') $('#page').innerHTML = heading() + `<section class="panel">${empty('暂时无法读取数据', errorText(error), '<button class="btn" data-reload>重新加载</button>')}</section>`; }
  }
  function focusSearch() { if (!state.token) return; if (state.route === 'memories') $('#memory-search')?.focus(); else { location.hash = 'memories'; setTimeout(() => $('#memory-search')?.focus(), 150); } }
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
      catch (error) { if (error.name !== 'AbortError' && state.token) { errorEl.textContent = errorText(error); button.disabled = false; } }
    };
  }
  async function remove(id) {
    const epoch = state.epoch; let m;
    try { m = (await api(`/v1/memories/${encodeURIComponent(id)}`)).memory; } catch (error) { if (error.name !== 'AbortError') toast(errorText(error), true); return; }
    if (epoch !== state.epoch) return;
    const dialog = openDialog('confirm', `<div class="dialog-head"><div><div class="eyebrow">DELETE MEMORY</div><h2>删除这段记忆？</h2></div><button class="icon-btn" data-close="confirm" aria-label="取消删除">${icon('close')}</button></div><div class="dialog-body"><div class="delete-preview prewrap">${esc(m.content.slice(0, 180))}${m.content.length > 180 ? '…' : ''}</div><p class="subheading">删除后不再参与召回。原始来源与修订记录会保留，便于追溯。</p><div id="delete-error" class="error-inline spaced" role="alert"></div></div><div class="dialog-footer"><button class="btn" data-close="confirm">保留记忆</button><button class="btn danger" id="confirm-delete">确认删除</button></div>`);
    $('#confirm-delete', dialog).onclick = async event => { const button = event.currentTarget; button.disabled = true; try { await api(`/v1/memories/${encodeURIComponent(id)}?expected_revision=${m.revision}`, { method: 'DELETE' }); closeDialog('confirm'); closeDialog('detail'); state.detail = null; toast('记忆已删除，旧任务不会恢复它。'); route(); } catch (error) { if (error.name !== 'AbortError' && state.token) { $('#delete-error').textContent = errorText(error); button.disabled = false; } } };
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
    $('#page').insertAdjacentHTML('beforeend', `<section class="panel job-card spaced"><div class="between"><div><h2>QQ 机器人</h2><p class="subheading">${qq.enabled ? `AppID ${esc(qq.app_id)} · 单聊接入 · HTTPS 回调路径 /qqbot` : '在 .env 填写 QQ AppID 和 AppSecret 后启用'}</p></div><span class="badge ${qq.bound ? 'green' : 'amber'}">${qq.bound ? '已绑定本人' : qq.enabled ? '等待绑定' : '未启用'}</span></div>${qq.enabled ? `<div class="between spaced"><small>待处理 ${fmt(qq.queue.pending)} · 运行中 ${fmt(qq.queue.running)} · 已发送 ${fmt(qq.queue.sent)} · 失败 ${fmt(qq.queue.dead)}</small>${qq.bound ? '<small>只接收已绑定账号的单聊消息</small>' : '<button class="btn small" id="qq-pair">生成一次性绑定码</button>'}</div><p class="hint spaced" id="qq-pair-code" role="status"></p>` : ''}</section>`);
    const button = $('#qq-pair');
    if (button) button.onclick = async () => {
      button.disabled = true;
      try {
        const pair = await send('/v1/qq/pairing', {});
        if (epoch !== state.epoch) return;
        const element = $('#qq-pair-code');
        element.textContent = `在 QQ 私聊发送：/绑定 ${pair.code}（10 分钟内有效，使用后作废）`;
        clearTimeout(state.poll);
        state.poll = setTimeout(() => { if (epoch === state.epoch && $('#qq-pair-code')) $('#qq-pair-code').textContent = '绑定码已过期，请重新生成。'; }, Math.max(0, pair.expires_at * 1000 - Date.now()));
      } catch (error) { if (error.name !== 'AbortError') toast(errorText(error), true); }
      finally { if (epoch === state.epoch) button.disabled = false; }
    };
  }
  function uptime(seconds) { const hours = Math.floor(seconds / 3600); return hours >= 24 ? `${Math.floor(hours / 24)} 天 ${hours % 24} 时` : hours ? `${hours} 时 ${Math.floor(seconds % 3600 / 60)} 分` : `${Math.floor(seconds / 60)} 分`; }
  function bytes(n) { return n >= 1024 ** 3 ? `${(n / 1024 ** 3).toFixed(2)} GB` : n >= 1024 ** 2 ? `${(n / 1024 ** 2).toFixed(1)} MB` : `${(n / 1024).toFixed(0)} KB`; }
  document.addEventListener('click', event => {
    const target = event.target.closest('button,[data-detail]'); if (!target || target.disabled) return;
    if (target.hasAttribute('data-new')) edit();
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
  document.addEventListener('keydown', event => { if (event.key === '/' && state.token && !['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement.tagName) && !$$('dialog[open]').length) { event.preventDefault(); focusSearch(); } });
  for (const type of ['pointerdown', 'keydown']) document.addEventListener(type, () => { lockIfIdle(); state.lastActivity = Date.now(); }, { passive: true });
  document.addEventListener('visibilitychange', () => { if (!document.hidden) lockIfIdle(); });
  window.addEventListener('pagehide', () => { if (state.token) logout(); });
  setInterval(lockIfIdle, 30000);
  login();
})();
