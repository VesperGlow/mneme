// Real Console/SQLite, with only a local streaming provider fixture.
// Build `cargo build --example console_fixture` first; no production services.
const { chromium } = require('playwright-core');
const assert = require('node:assert/strict');
const http = require('node:http');
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const BASE = 'http://127.0.0.1:18080';

(async () => {
  const provider = http.createServer(async (request, response) => {
    let raw = ''; for await (const part of request) raw += part;
    const body = JSON.parse(raw);
    response.writeHead(200, { 'content-type': 'text/event-stream' });
    response.write(`data: ${JSON.stringify({ choices: [{ delta: { content: '临时测试输出' }, finish_reason: null }], usage: { prompt_tokens: 10, completion_tokens: 2, total_tokens: 12 } })}\n\n`);
    if (!body.messages.at(-1).content.includes('等待取消测试')) response.end('data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
  });
  await new Promise(resolve => provider.listen(0, '127.0.0.1', resolve));
  let fixture; let browser;
  try {
    try { await fetch(`${BASE}/health/live`); throw new Error('Port 18080 is already occupied; refusing to use an unknown instance.'); }
    catch (error) { if (!error.cause) throw error; }
    fixture = spawn('target/debug/examples/console_fixture', [], { env: { ...process.env, MNEME_LLM_UI_URL: `http://127.0.0.1:${provider.address().port}` }, stdio: ['ignore', 'ignore', 'pipe'] });
    let logs = ''; fixture.stderr.on('data', data => { logs += data; });
    for (let attempt = 0; attempt < 100; attempt++) {
      if (fixture.exitCode != null) throw new Error(`Fixture exited: ${logs}`);
      try { if ((await fetch(`${BASE}/health/live`)).ok) break; } catch {}
      await new Promise(resolve => setTimeout(resolve, 100));
    }
    browser = await chromium.launch({ executablePath: process.env.CHROMIUM_PATH || '/usr/bin/chromium', headless: true, args: ['--no-sandbox', '--disable-dev-shm-usage'] });
    const page = await browser.newPage({ viewport: { width: 1280, height: 1000 } });
    const errors = []; page.on('pageerror', error => errors.push(error.message));
    await page.goto(`${BASE}/console/`);
    await page.locator('#password').fill('console-fixture-password');
    await page.getByRole('button', { name: '解锁控制台' }).click();
    await page.locator('#chat-form').waitFor();
    assert.equal(await page.locator('#stop-chat').isVisible(), false);
    for (const reload of [false, true]) {
      await page.locator('#chat-message').fill('等待取消测试'); await page.locator('#send-chat').click();
      await page.locator('#chat-answer').getByText('临时测试输出').waitFor();
      if (reload) { await page.reload(); await page.locator('.chat-resuming').waitFor(); }
      await page.locator('#stop-chat').click();
      await page.waitForFunction(() => !document.querySelector('#chat-message')?.disabled);
      assert.equal(await page.locator('#stop-chat').isVisible(), false);
      assert.equal(await page.locator('.chat-message.assistant').filter({ hasText: '临时测试输出' }).count(), 0);
      assert((await page.locator('#chat-messages').innerText()).includes('已停止生成'));
    }
    const content = '用户住在成都 QA <img src=x onerror="window.__forget_xss=1">';
    const createdResponse = await page.request.post(`${BASE}/v1/memories`, { data: { content, kind: 'fact', importance: 0.8, confidence: 1, entities: [], valid_until: null } });
    assert.equal(createdResponse.status(), 201); const created = (await createdResponse.json()).memory;
    await page.locator('#chat-message').fill('请忘记成都'); await page.locator('#send-chat').click();
    await page.locator('[name="forget-memory"]').first().waitFor();
    assert.equal(await page.locator('#forget-submit').isEnabled(), false);
    assert.equal(await page.locator('#confirm img').count(), 0);
    assert.equal(await page.evaluate(() => window.__forget_xss), undefined);
    const selected = page.locator(`[name="forget-memory"][value="${created.id}"]`);
    await selected.check();
    await page.route('**/v1/memories/forget/confirm', route => route.fulfill({ status: 409, json: { error: 'forget_preview_expired' } }), { times: 1 });
    await page.locator('#forget-submit').click();
    await page.locator('#forget-error').getByText('预览已过期，请重新查找并核对。').waitFor();
    const before = await (await page.request.get(`${BASE}/v1/memories/${created.id}`)).json();
    assert.equal(before.memory.deleted_at, null);
    await page.locator('#forget-search').click();
    await page.locator(`[name="forget-memory"][value="${created.id}"]`).check();
    await page.locator('#forget-scope').selectOption('context');
    await page.setViewportSize({ width: 390, height: 844 });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
    fs.mkdirSync('artifacts/optimization-2026-10-23', { recursive: true });
    await page.screenshot({ path: 'artifacts/optimization-2026-10-23/forget-mobile.png', fullPage: true });
    await page.locator('#forget-submit').click();
    await page.locator('#confirm').waitFor({ state: 'hidden' });
    const after = await (await page.request.get(`${BASE}/v1/memories/${created.id}`)).json();
    assert(after.memory.deleted_at);
    const conversation = await (await page.request.get(`${BASE}/v1/conversation`)).json();
    assert.equal(conversation.context.summary, ''); assert.equal(conversation.context.history_turns, 0);
    assert.deepEqual(errors, []);
    console.log('Control WebUI passed: live/resumed cancellation, confirmed forgetting, expiry, XSS and 390px layout.');
  } finally {
    if (browser) await browser.close();
    if (fixture && fixture.exitCode == null) { fixture.kill('SIGTERM'); await new Promise(resolve => fixture.once('exit', resolve)); }
    provider.closeAllConnections(); await new Promise(resolve => provider.close(resolve));
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
