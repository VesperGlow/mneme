// Isolated browser benchmark. Synthetic history and SSE; no credentials or model calls.
// NODE_PATH=/tmp/mneme-console-qa/node_modules node scripts/chat-performance.cjs
const { chromium } = require('playwright-core');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const http = require('node:http');
const crypto = require('node:crypto');
const root = path.resolve(__dirname, '..');
const out = path.resolve(process.env.MNEME_PERF_OUTPUT || 'artifacts/chat-performance.json');
const samples = Number(process.env.MNEME_PERF_SAMPLES || 120);
const turns = Number(process.env.MNEME_PERF_TURNS || 200);
const variants = (process.env.MNEME_PERF_VARIANTS || 'before,after').split(',');
const keys = ['input_tokens', 'output_tokens', 'reasoning_tokens', 'total_tokens', 'cached_input_tokens', 'cache_creation_input_tokens'];
const usage = (n = 0) => ({ input_tokens: 1200 + n, output_tokens: 300, reasoning_tokens: 200, total_tokens: 1500 + n, cached_input_tokens: 512, cache_creation_input_tokens: null, source: 'api', estimated: { tokenizer: 'fixture-tokenizer', model_matched: false } });
const aggregate = (requests, n = 0) => ({ requests, incomplete_requests: 0, output_lower_bound_requests: 0,
  ...Object.fromEntries(keys.map(k => [k, usage(n)[k] == null ? null : usage(n)[k] * requests])),
  effective: Object.fromEntries(keys.map(k => [k, (usage(n)[k] || 0) * requests])),
  reported: Object.fromEntries(keys.map(k => [k, k === 'cache_creation_input_tokens' ? 0 : requests])),
  estimated: Object.fromEntries(keys.map(k => [k, 0])), by_purpose: {} });
const prose = '先核对已有记忆，再检查时间、关系和来源。 Keep the original facts and explain the evidence clearly.\n\n';
const reasoning = prose.repeat(24);
function history() {
  const h = { messages: [], model_requests: [], tool_calls: [], assets: [], stickers: [], turn_usage: {}, conversation: { active_turn: null, context: {
    context_tokens: 100000, context_used_tokens: 32000, tokenizer: 'fixture-tokenizer', distribution: { system: 2000, history: 30000 },
    reply_reserve_tokens: 4000, tool_reserve_tokens: 1000, compactions: 2, memory_extract_tokens: 2048, memory_extract_max_delay_seconds: 120, summary: 'Synthetic performance fixture', session_usage: aggregate(turns * 4),
  } } };
  for (let i = 0; i < turns; i++) addTurn(h, `turn-${i}`, i);
  return h;
}
function addTurn(h, id, i) {
  h.messages.push({ id: `${id}-user`, turn_id: id, role: 'user', channel: 'web', status: 'completed', content: `第 ${i + 1} 轮：请检查已有事实，并给出清晰的说明。`, created_at: 1700000000 + i });
  h.messages.push({ id: `${id}-answer`, turn_id: id, role: 'assistant', channel: 'web', status: 'completed', content: prose.repeat(3) + '- 保留有效事实\n- 核对时间与关系\n\n`memory_id` 与 [来源](https://example.org/)', created_at: 1700000000 + i });
  for (const [j, purpose] of ['chat', 'embedding', 'reranker', 'memory'].entries()) h.model_requests.push({ id: `${id}-request-${j}`, turn_id: id, purpose, model: 'fixture-model', status: 'completed', context: { system: 100, history: 1100 }, usage: usage(), reasoning: j ? '' : reasoning });
  h.turn_usage[id] = aggregate(4);
}
const sessions = new Map();
const assets = new Map();
for (const variant of variants) {
  const dir = variant === 'before' ? 'artifacts/mneme-ui-perf-before' : 'web';
  assets.set(variant, Object.fromEntries(['app.js', 'style.css'].map(f => [f, fs.readFileSync(path.join(root, dir, f))])));
}
const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const session = sessions.get(req.headers['x-bench-session']);
  const json = data => { res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify(data)); };
  if (url.pathname === '/console/') {
    const html = fs.readFileSync(path.join(root, 'web/index.html'), 'utf8').replaceAll('/console/app.js', `/bench/${session.variant}/app.js`).replaceAll('/console/style.css', `/bench/${session.variant}/style.css`);
    res.writeHead(200, { 'content-type': 'text/html' }); return res.end(html);
  }
  const match = url.pathname.match(/^\/bench\/(\w+)\/(app\.js|style\.css)$/);
  if (match) {
    res.writeHead(200, { 'content-type': match[2].endsWith('js') ? 'text/javascript' : 'text/css' });
    const body = assets.get(match[1])[match[2]];
    return res.end(match[2] === 'app.js' ? body.toString().replace("} catch (error) { if (error.name === 'AbortError' || epoch !== state.epoch) return; }", "} catch (error) { window.__benchPollErrors.push(error.stack); if (error.name === 'AbortError' || epoch !== state.epoch) return; }") : body);
  }
  if (url.pathname === '/console/icon.svg') { res.writeHead(200, { 'content-type': 'image/svg+xml' }); return res.end(fs.readFileSync(path.join(root, 'web/icon.svg'))); }
  if (url.pathname === '/v1/console/auth') return json({});
  if (url.pathname === '/v1/console/tools') return json({ enabled: false });
  if (url.pathname === '/v1/console/messages') return json(session.data);
  if (url.pathname === '/v1/chat') {
    const chunks = []; for await (const chunk of req) chunks.push(chunk);
    const input = JSON.parse(Buffer.concat(chunks));
    res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-store' });
    const emit = (event, data) => res.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
    const request = `${input.request_id}-request-0`;
    emit('context', { request_id: request, model: 'fixture-model', purpose: 'chat', context: { system: 100, history: 1100 }, estimated_input_tokens: 1200 });
    emit('reasoning', { request_id: request, content: 'Check the facts. ' });
    emit('turn_usage', { turn_id: input.request_id, usage: aggregate(1) });
    await new Promise(resolve => setTimeout(resolve, 400));
    for (let i = 0; i < 160; i++) {
      emit('reasoning', { request_id: request, content: `Evidence${i}`.repeat(24) });
      emit('delta', { content: `回答 ${i}：${prose}` });
      if (!(i % 8)) { emit('turn_usage', { turn_id: input.request_id, usage: aggregate(1, i) }); await new Promise(resolve => setTimeout(resolve, 8)); }
    }
    addTurn(session.data, input.request_id, session.data.messages.length / 2);
    session.data.model_requests = session.data.model_requests.filter(r => r.turn_id !== input.request_id || r.id === request);
    const saved = session.data.model_requests.find(r => r.id === request);
    saved.reasoning = 'Check the facts. ' + Array.from({ length: 160 }, (_, i) => `Evidence${i}`.repeat(24)).join('');
    saved.usage = usage(159);
    session.data.messages.at(-1).content = Array.from({ length: 160 }, (_, i) => `回答 ${i}：${prose}`).join('');
    session.data.turn_usage[input.request_id] = aggregate(1, 159);
    emit('usage', { request_id: request, status: 'completed', usage: usage(159) });
    emit('turn_usage', { turn_id: input.request_id, usage: aggregate(1, 159) });
    emit('done', { persisted: true, turn_id: input.request_id });
    return res.end();
  }
  res.writeHead(404); res.end();
});
function summary(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const q = p => sorted[Math.max(0, Math.ceil(sorted.length * p) - 1)];
  return { samples: values.length, p50_ms: q(.5), p95_ms: q(.95), p99_ms: q(.99), max_ms: sorted.at(-1), raw_ms: values };
}
const results = { methodology: { samples, warmups: 8, turns, messages: turns * 2, requests_per_turn: 4, synthetic: true, paid_api_calls: 0, quantiles: 'nearest rank; empirical sample quantiles, not confidence bounds', timing: 'interaction: DOM click to two animation frames; refresh includes loopback JSON + browser update + forced layout; scroll: rAF interval while scrolling', mobile: '390px viewport, Chromium CDP CPU slowdown 4x; not a physical phone', harness_sha256: crypto.createHash('sha256').update(fs.readFileSync(__filename)).digest('hex'), node: process.version }, assets: Object.fromEntries([...assets].map(([name, files]) => [name, Object.fromEntries(Object.entries(files).map(([f, bytes]) => [f, crypto.createHash('sha256').update(bytes).digest('hex')]))])), runs: [] };
async function run(browser, variant, mobile) {
  const id = crypto.randomUUID(); const data = history(); const session = { variant, data }; sessions.set(id, session);
  const context = await browser.newContext({ viewport: { width: mobile ? 390 : 1440, height: mobile ? 844 : 1000 }, isMobile: mobile, hasTouch: mobile, reducedMotion: 'reduce', locale: 'zh-CN', extraHTTPHeaders: { 'x-bench-session': id } });
  const page = await context.newPage(); const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const cdp = await context.newCDPSession(page); if (mobile) await cdp.send('Emulation.setCPUThrottlingRate', { rate: 4 });
  await page.addInitScript(() => {
    const timeout = window.setTimeout;
    window.setTimeout = (fn, ms, ...args) => ms === 3000 ? (window.__benchPoll = () => fn(...args), 0) : timeout(fn, ms, ...args);
    window.__benchStart = performance.now();
    window.__benchLongTasks = [];
    window.__benchPollErrors = [];
    new PerformanceObserver(list => window.__benchLongTasks.push(...list.getEntries().map(entry => entry.duration))).observe({ type: 'longtask', buffered: true });
  });
  await page.goto(`http://127.0.0.1:${server.address().port}/console/`);
  await page.waitForFunction(n => document.querySelectorAll('.chat-message').length === n, turns * 2, { timeout: 120000 });
  const run = { variant, device: mobile ? 'mobile_4x' : 'desktop', initial: await page.evaluate(() => ({ ready_ms: performance.now() - window.__benchStart, dom_nodes: document.querySelectorAll('*').length })), checks: {} };
  const interaction = async selector => page.evaluate(async selector => {
    const detail = document.querySelector(selector); detail.scrollIntoView({ block: 'center' });
    const start = performance.now(); detail.querySelector('summary').click();
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    detail.getBoundingClientRect(); return performance.now() - start;
  }, selector);
  for (const [name, card] of [['reasoning', '.chat-reasoning'], ['tokens', '.turn-usage']]) {
    const open = [], close = [];
    for (let i = 0; i < samples + 8; i++) {
      const selector = `.chat-message.assistant:nth-of-type(${2 * (i % turns) + 2}) ${card}`;
      const a = await interaction(selector); const b = await interaction(selector);
      if (i >= 8) { open.push(a); close.push(b); }
    }
    run[`${name}_open`] = summary(open); run[`${name}_close`] = summary(close);
  }
  const refresh = [];
  for (let i = 0; i < samples + 8; i++) {
    const last = session.data.model_requests.at(-1); last.usage = usage(i); session.data.turn_usage[last.turn_id] = aggregate(4, i);
    const result = await page.evaluate(async () => { const at = performance.now(); await window.__benchPoll(); document.querySelector('#chat-messages').getBoundingClientRect(); return { ms: performance.now() - at, total: [...document.querySelectorAll('.turn-usage > summary .usage-value')].at(-1).textContent }; });
    assert(result.total.includes((6000 + 4 * i).toLocaleString('zh-CN')), 'refresh must display updated server totals');
    if (i >= 8) refresh.push(result.ms);
  }
  run.refresh = summary(refresh);
  const scrollFrames = await page.evaluate(async n => {
    const el = document.querySelector('#chat-messages'); let previous; const frames = [];
    for (let i = 0; i < n + 10; i++) {
      const at = await new Promise(requestAnimationFrame); if (previous && i >= 10) frames.push(at - previous); previous = at;
      el.scrollTop = (el.scrollHeight - el.clientHeight) * (1 + Math.sin(i / 20)) / 2;
    }
    return frames;
  }, samples);
  run.scroll_frames = summary(scrollFrames);
  results.runs.push(run);
  fs.mkdirSync(path.dirname(out), { recursive: true }); fs.writeFileSync(out, JSON.stringify(results, null, 2));
  console.log(JSON.stringify({ phase: 'timings', variant, device: run.device, initial: run.initial, refresh_p99_ms: run.refresh.p99_ms }));
  // Adversarial layout, keyboard accessibility, anchoring and real stream handler.
  const last = session.data.model_requests.findLast(r => r.purpose === 'chat');
  last.reasoning = 'Supercalifragilisticexpialidocious'.repeat(10000);
  last.model = 'LongModelIdentifier'.repeat(100);
  session.data.turn_usage[last.turn_id] = aggregate(4);
  for (const k of keys) { session.data.turn_usage[last.turn_id][k] = Number.MAX_SAFE_INTEGER; session.data.turn_usage[last.turn_id].effective[k] = Number.MAX_SAFE_INTEGER; }
  await page.evaluate(() => window.__benchPoll());
  for (const width of mobile ? [390, 320, 768] : [1440, 1024]) {
    await page.setViewportSize({ width, height: mobile ? 844 : 1000 });
    const answer = page.locator('.chat-message.assistant').last();
    await answer.locator('.chat-reasoning > summary').click();
    await answer.locator('.reasoning-body').waitFor({ state: 'visible' });
    await answer.locator('.turn-usage > summary').click();
    run.checks[`layout_${width}`] = await answer.evaluate(el => ({ overflow: document.documentElement.scrollWidth > innerWidth || el.scrollWidth > el.clientWidth + 1, reasoning_overflow: el.querySelector('.reasoning-body').scrollWidth > el.querySelector('.reasoning-body').clientWidth + 1, internal_scroll: getComputedStyle(el.querySelector('.reasoning-body')).overflowY !== 'visible', summary_overflow: el.querySelector('.turn-usage > summary').scrollWidth > el.querySelector('.turn-usage > summary').clientWidth + 1, reasoning_truncated: el.querySelector('.reasoning-body').textContent.length !== 340000 }));
    await answer.locator('.chat-reasoning > summary').click(); await answer.locator('.turn-usage > summary').click();
  }
  await page.setViewportSize({ width: mobile ? 390 : 1440, height: mobile ? 844 : 1000 });
  const longOpen = [], longClose = [];
  for (let i = 0; i < samples + 8; i++) {
    const a = await interaction('.chat-message.assistant:last-child .chat-reasoning');
    const b = await interaction('.chat-message.assistant:last-child .chat-reasoning');
    if (i >= 8) { longOpen.push(a); longClose.push(b); }
  }
  run.long_english_open = summary(longOpen); run.long_english_close = summary(longClose);
  run.checks.token_cases = {};
  for (const kind of ['no_requests', 'zero', 'unknown', 'estimated', 'mixed', 'unsafe_integer']) {
    const totals = aggregate(kind === 'no_requests' ? 0 : 4);
    for (const key of keys) {
      if (kind === 'zero') { totals[key] = 0; totals.effective[key] = 0; totals.reported[key] = 4; }
      if (kind === 'unknown') { totals[key] = null; totals.effective[key] = 0; totals.reported[key] = 0; }
      if (kind === 'estimated') { totals[key] = null; totals.effective[key] = 123456; totals.reported[key] = 0; totals.estimated[key] = 4; }
      if (kind === 'mixed') { totals[key] = 100; totals.effective[key] = 150; totals.reported[key] = 2; totals.estimated[key] = 1; totals.output_lower_bound_requests = 1; }
      if (kind === 'unsafe_integer') { totals[key] = 9223372036854776000; totals.effective[key] = 9223372036854776000; totals.reported[key] = 4; }
    }
    session.data.turn_usage[last.turn_id] = totals;
    await page.evaluate(() => window.__benchPoll());
    const panel = page.locator('.chat-message.assistant').last().locator('.turn-usage');
    const text = await panel.locator(':scope > summary').innerText();
    await panel.locator(':scope > summary').click();
    const detailText = await panel.innerText();
    run.checks.token_cases[kind] = { summary: text, no_invalid_numbers: !/NaN|Infinity|undefined/.test(detailText), expected_marker: kind === 'unknown' ? text.includes('未提供') : ['estimated', 'mixed', 'unsafe_integer'].includes(kind) ? text.includes('≈') : /输入\s+0/.test(text) };
    await panel.locator(':scope > summary').click();
  }
  await page.setViewportSize({ width: mobile ? 390 : 1440, height: mobile ? 844 : 1000 });
  const anchor = page.locator('.chat-message.assistant').nth(40);
  await anchor.scrollIntoViewIfNeeded(); const top = (await anchor.boundingBox()).y;
  session.data.model_requests.at(-1).usage = usage(777);
  await page.evaluate(() => window.__benchPoll());
  run.checks.scroll_anchor_delta_px = Math.abs((await anchor.boundingBox()).y - top);
  const anchorHandle = await anchor.elementHandle();
  session.data.model_requests.at(-1).usage = usage(778);
  await page.evaluate(() => window.__benchPoll());
  run.checks.unchanged_message_preserved = await anchorHandle.evaluate(el => el.isConnected);
  const saved = page.locator('.chat-message.assistant').last();
  await saved.locator('.turn-usage > summary').focus(); await page.keyboard.press('Enter');
  run.checks.keyboard_details_open = await saved.locator('.turn-usage').evaluate(el => el.open);
  await page.keyboard.press('Enter');
  const streamDurations = [];
  for (let i = 0; i < 3; i++) {
  const at = Date.now();
  await page.locator('#chat-message').fill('连续流式对话'); await page.locator('#send-chat').click();
  await page.waitForFunction(() => { const summary = document.querySelector('#chat-live-requests .chat-reasoning > summary'); if (summary) { summary.click(); return true; } });
  await page.waitForFunction(() => { const summary = document.querySelector('#chat-live-usage .turn-usage > summary'); if (summary) { summary.click(); return true; } });
  await page.locator('#chat-message:not([disabled])').waitFor({ timeout: 120000 });
  run.checks.stream_reasoning_open = await page.locator('.chat-message.assistant').last().locator('.chat-reasoning').evaluate(el => el.open);
  run.checks.stream_usage_open = await page.locator('.chat-message.assistant').last().locator('.turn-usage').evaluate(el => el.open);
  run.checks.stream_reply_complete = (await page.locator('.chat-message.assistant').last().locator('.answer-body').innerText()).includes('回答 159');
  streamDurations.push(Date.now() - at);
  }
  run.continuous_stream_ms = streamDurations;
  run.checks.message_count = await page.locator('.chat-message').count();
  run.long_tasks = await page.evaluate(() => window.__benchLongTasks);
  run.checks.poll_errors = await page.evaluate(() => window.__benchPollErrors);
  fs.writeFileSync(out, JSON.stringify(results, null, 2));
  if (variant === 'after') {
    for (const [key, check] of Object.entries(run.checks)) if (key.startsWith('layout_')) assert(!Object.values(check).some(Boolean), key);
    for (const [kind, check] of Object.entries(run.checks.token_cases)) assert(check.no_invalid_numbers && check.expected_marker, kind);
    assert(run.checks.keyboard_details_open && run.checks.stream_reasoning_open && run.checks.stream_usage_open && run.checks.stream_reply_complete);
    assert(run.checks.scroll_anchor_delta_px <= 2);
    assert.equal(run.checks.message_count, turns * 2 + 6);
    assert(run.checks.unchanged_message_preserved);
    assert.deepEqual(run.checks.poll_errors, []);
  }
  assert.deepEqual(errors, []); run.checks.javascript_errors = errors;
  fs.writeFileSync(out, JSON.stringify(results, null, 2));
  console.log(JSON.stringify({ variant, device: run.device, initial: run.initial, refresh: { p95: run.refresh.p95_ms, p99: run.refresh.p99_ms }, checks: run.checks }));
  await context.close(); sessions.delete(id);
}
(async () => {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const browser = await chromium.launch({ executablePath: process.env.CHROMIUM_PATH || '/usr/bin/chromium', headless: true, args: ['--no-sandbox', '--disable-dev-shm-usage'] });
  try { for (const mobile of [false, true]) for (const variant of variants) await run(browser, variant, mobile); }
  finally { await browser.close(); await new Promise(resolve => server.close(resolve)); }
})().catch(error => { console.error(error); process.exitCode = 1; server.closeAllConnections(); server.close(); });
