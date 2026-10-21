// Run against examples/console_fixture. The state response is a UI fixture;
// tests/interaction.rs exercises real source continuity and expiry.
const { chromium } = require('playwright-core');
const assert = require('node:assert/strict');

(async () => {
  const browser = await chromium.launch({ executablePath: '/usr/bin/chromium', headless: true, args: ['--no-sandbox', '--disable-dev-shm-usage'] });
  try {
    const page = await browser.newPage({ viewport: { width: 1280, height: 1000 } });
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    let snapshot = { updated_at: Math.floor(Date.now()/1000), state: {
      context: { text: '<img src=x onerror="window.__state_xss=1">先聊结局', source_id: 'source-1', quote: '结局等一会儿再说 <script>window.__state_xss=1</script>' },
      open_thread: null,
    } };
    let busy = true;
    await page.route('**/v1/console/persona/state', async route => {
      if (route.request().method() === 'DELETE') {
        if (busy) return route.fulfill({ status: 409, contentType: 'application/json', body: JSON.stringify({ error: 'interaction_turn_running' }) });
        snapshot = null;
        return route.fulfill({ contentType: 'application/json', body: JSON.stringify({ cleared: true }) });
      }
      return route.fulfill({ contentType: 'application/json', body: JSON.stringify({ snapshot:null, mode:"history_and_summary", summary:"展览的话题待续。", legacy_notes:snapshot }) });
    });
    await page.goto('http://127.0.0.1:18080/console/#agent');
    await page.locator('#password').fill('console-fixture-password');
    await page.getByRole('button', { name: '解锁控制台' }).click();
    const panel = page.locator('#interaction-panel');
    await panel.getByRole('button', { name: '清除旧笔记' }).waitFor();
    await panel.locator('summary').click();
    assert.match(await panel.innerText(), /先聊结局/);
    assert.equal(await page.evaluate(() => window.__state_xss), undefined);
    await panel.getByRole('button', { name: '清除旧笔记' }).click();
    await panel.getByText('当前正在回复，请等待本轮完成后清除临时状态。').waitFor();
    busy = false;
    await panel.getByRole('button', { name: '清除旧笔记' }).click();
    await panel.getByText('展览的话题待续。').waitFor();
    await panel.getByRole('button', { name: '清除旧笔记' }).waitFor({ state: 'detached' });
    await page.setViewportSize({ width: 390, height: 844 });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), true);
    assert.deepEqual(errors, []);
    console.log('Continuity WebUI: summary, read-only legacy notes, XSS, busy clear conflict and mobile passed.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
