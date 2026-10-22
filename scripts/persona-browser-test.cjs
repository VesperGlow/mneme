// Run against examples/console_fixture on :18080.
const { chromium } = require('playwright-core');
const assert = require('node:assert/strict');
const fs = require('node:fs');

const BASE = 'http://127.0.0.1:18080';
const output = 'artifacts/persona-single-field';
fs.mkdirSync(output, { recursive: true });

(async () => {
  const browser = await chromium.launch({ executablePath: '/usr/bin/chromium', headless: true, args: ['--no-sandbox', '--disable-dev-shm-usage'] });
  try {
    const context = await browser.newContext({ viewport: { width: 1280, height: 1000 } });
    const page = await context.newPage();
    const errors = [];
    const previewRequests = [];
    page.on('pageerror', error => errors.push(error.message));
    page.on('request', request => {
      if (/\/persona\/(context|preview|archive)$/.test(new URL(request.url()).pathname)) previewRequests.push(request.url());
    });
    await page.goto(`${BASE}/console/#agent`);
    await page.locator('#password').fill('console-fixture-password');
    await page.getByRole('button', { name: '解锁控制台' }).click();
    await page.locator('#interaction-panel button').first().waitFor();
    const original = await (await context.request.get(`${BASE}/v1/console/settings`)).json();
    const seed = {
      ...original.persona,
      name: '白石雫 <img src=x onerror="window.__persona_xss=1">',
      style: '自然、松弛。',
      boundaries: '不编造记忆，不确定时说明。',
      output_format: '纯文本。',
    };
    assert.equal((await context.request.put(`${BASE}/v1/console/persona`, { data: seed })).status(), 200);
    await page.reload();
    const panel = page.locator('[data-setting="persona"]');
    await panel.locator(':scope > summary').waitFor();
    if (!await panel.evaluate(el => el.open)) await panel.locator(':scope > summary').click();
    assert.equal(await page.locator('#persona-form textarea').count(), 1);
    assert.equal(await page.locator('#persona-form input').count(), 0);
    assert.equal(await page.locator('#persona-context,#persona-preview,#persona-archive').count(), 0);
    assert.equal(await page.getByRole('heading', { name: '角色预览', exact: true }).count(), 0);
    assert.equal(await page.locator('#agent-name').innerText(), seed.name);
    const description = '沉静、独立，珍视坦诚；与用户是朋友。\n字面文本 </textarea><img src=x onerror="window.__persona_xss=1">';
    await page.locator('#persona-description').fill(description);
    const saved = page.waitForResponse(response => new URL(response.url()).pathname === '/v1/console/persona' && response.request().method() === 'PUT');
    await page.getByRole('button', { name: '保存性格', exact: true }).click();
    assert.deepEqual((await (await saved).json()).persona, { ...seed, description });
    await page.getByText('性格描述已保存，下一轮生效。', { exact: true }).waitFor();
    await page.reload();
    await panel.locator(':scope > summary').waitFor();
    if (!await panel.evaluate(el => el.open)) await panel.locator(':scope > summary').click();
    assert.equal(await page.locator('#persona-description').inputValue(), description);
    assert.equal(await page.locator('#agent-name').innerText(), seed.name);
    assert.equal(await page.locator('#persona-form img,.agent-profile img').count(), 0);
    assert.equal(await page.evaluate(() => window.__persona_xss), undefined);
    await page.getByRole('button', { name: '填入默认描述', exact: true }).click();
    assert.equal(await page.locator('#persona-description').inputValue(), original.default_persona.description);
    assert.deepEqual((await (await context.request.get(`${BASE}/v1/console/settings`)).json()).persona, { ...seed, description }, 'defaults must remain an unsaved draft');
    const savedDefault = page.waitForResponse(response => new URL(response.url()).pathname === '/v1/console/persona' && response.request().method() === 'PUT');
    await page.getByRole('button', { name: '保存性格', exact: true }).click();
    assert.equal((await savedDefault).status(), 200);
    assert.deepEqual((await (await context.request.get(`${BASE}/v1/console/settings`)).json()).persona, { ...seed, description: original.default_persona.description });
    assert.deepEqual(previewRequests, []);
    await page.screenshot({ path: `${output}/desktop.png`, fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth), true);
    await page.screenshot({ path: `${output}/mobile.png`, fullPage: true });
    assert.deepEqual(errors, []);
    assert.equal((await context.request.put(`${BASE}/v1/console/persona`, { data: original.persona })).status(), 200);
    console.log('PASS: one description field, saved/reloaded/default drafts, preserved name and legacy fields, no preview requests, XSS and mobile.');
  } finally { await browser.close(); }
})().catch(error => { console.error(error); process.exitCode = 1; });
