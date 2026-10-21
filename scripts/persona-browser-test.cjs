// Run against examples/console_fixture on :18080.
const { chromium } = require('playwright-core');
const assert = require('node:assert/strict');
(async () => {
 const browser = await chromium.launch({executablePath:'/usr/bin/chromium',headless:true,args:['--no-sandbox','--disable-dev-shm-usage']});
 try {
  const context=await browser.newContext({viewport:{width:1280,height:1000}});const page=await context.newPage();const errors=[];
  page.on('pageerror',error=>errors.push(error.message));
  await page.goto('http://127.0.0.1:18080/console/#agent');await page.locator('#password').fill('console-fixture-password');await page.getByRole('button',{name:'解锁控制台'}).click();
  await page.locator('#persona-context').waitFor();const raw=page.locator('[data-setting="persona"]');if(!await raw.getAttribute('open'))await raw.locator(':scope > summary').click();
  await page.locator('#persona-description').fill('沉静、独立，珍视坦诚；与用户是朋友。<img src=x onerror="window.__persona_xss=1">');
  await page.locator('#persona-preview [name="message"]').fill('今天看到一只猫。');await page.getByRole('button',{name:'预览回复',exact:true}).click();
  await page.getByText('请先保存角色背景，再预览。').waitFor();
  await page.getByRole('button',{name:'保存性格',exact:true}).click();
  await page.getByText('角色背景已保存，下一轮生效。').waitFor();
  const details=page.locator('#persona-context details').first();await details.locator('summary').click();
  assert.match(await details.locator('pre').innerText(),/沉静、独立/);assert.doesNotMatch(await details.locator('pre').innerText(),/derived_persona_profile|evolution_memory/);
  assert.equal(await page.locator('#persona-context img').count(),0);assert.equal(await page.evaluate(()=>window.__persona_xss),undefined);
  await page.locator('#persona-preview [name="message"]').fill('今天看到一只猫。');await page.getByRole('button',{name:'预览回复',exact:true}).click();
  await page.waitForFunction(()=>{const s=document.querySelector('#preview-answer')?.textContent;return s&&!s.includes('正在生成');});
  const history=await(await context.request.get('http://127.0.0.1:18080/v1/messages')).json();assert.equal(history.messages.length,0);
  await page.locator('#persona-archive > summary').click();await page.getByRole('button',{name:'查看归档'}).click();await page.waitForFunction(()=>document.querySelector('#persona-archive pre').textContent.includes('archived'));
  assert.equal(await page.getByRole('button',{name:'生成行为画像'}).count(),0);
  await page.screenshot({path:'artifacts/minimal-persona-baseline/persona-desktop.png',fullPage:true});await page.setViewportSize({width:390,height:844});assert.equal(await page.evaluate(()=>document.documentElement.scrollWidth<=window.innerWidth),true);
  await page.screenshot({path:'artifacts/minimal-persona-baseline/persona-mobile.png',fullPage:true});assert.deepEqual(errors,[]);
  console.log('PASS: saved background, no auto generation, draft guard, effective prompt, isolated preview, archive, XSS and mobile.');
 } finally {await browser.close();}
})().catch(error=>{console.error(error);process.exitCode=1;});
