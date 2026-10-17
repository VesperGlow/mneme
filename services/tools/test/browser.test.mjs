import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { BrowserTools } from '../browser.mjs';
import { TavilySearch } from '../search.mjs';
import { NetworkPolicy } from '../security.mjs';

test('session creation is bounded and a late context is disposed', async()=>{
  const browser=new BrowserTools({timeoutMs:50});let release,closed=0;
  browser.browser={isConnected:()=>true,newContext:()=>new Promise(resolve=>{release=resolve;})};
  try{
    await assert.rejects(browser.execute('11111111-1111-4111-8111-111111111111',{action:'navigate',url:'https://example.org'}),/browser_session_timeout/);
    assert.equal(browser.sessions.size,0);
    release({close:async()=>{closed++;}});await new Promise(resolve=>setImmediate(resolve));
    assert.equal(closed,1,'a context arriving after the deadline must not leak');
  }finally{browser.browser=null;await browser.close();}
});

test('real sandboxed CDP: Tavily search preserves the page, navigation and verification errors', {skip:process.env.MNEME_BROWSER_TEST!=='1',timeout:60000}, async()=>{
  class FixturePolicy extends NetworkPolicy {
    async resolve(input) {
      const url=new URL(input);
      if(url.hostname==='docs.example') return {url,host:url.hostname,port:443,address:'93.184.216.34',family:4};
      return super.resolve(input);
    }
  }
  const search=new TavilySearch({apiKeys:'tvly-fixture',fetchImpl:async()=>Response.json({results:[{title:'Source page',url:'https://docs.example/',content:'A source found by Tavily.'}]})});
  const browser=new BrowserTools({policy:new FixturePolicy(),search});
  const id='11111111-1111-4111-8111-111111111111';
  try {
    const found=await browser.execute(id,{action:'search',query:'source'});
    assert.equal(found.results[0].url,'https://docs.example/');
    assert.equal(browser.sessions.size,0,'search must not allocate a page');
    await assert.rejects(browser.execute(id,{action:'read'}),/browser_session_missing_navigate/);
    const session=await browser.session(id);
    await session.context.route('https://docs.example/**',async route=>{
      const path=new URL(route.request().url()).pathname;
      const status=path==='/denied'?403:path==='/rate-limited'?429:200;
      const body=path==='/verification'?'<title>Verification</title><p>Verify you are human.</p>':'<title>Source page</title><article><p>Readable source content.</p><a href="https://docs.example/next">Next page</a></article>';
      await route.fulfill({status,contentType:'text/html; charset=utf-8',body});
    });
    const opened=await browser.execute(id,{action:'navigate',url:found.results[0].url});
    const refs=[...session.refs.keys()];
    assert.equal(opened.title,'Source page');
    await browser.execute(id,{action:'search',query:'another source'});
    assert.equal(browser.sessions.get(id),session);
    assert.equal(session.page.url(),found.results[0].url);
    assert.deepEqual([...session.refs.keys()],refs,'search must preserve current element references');
    assert.match((await browser.execute(id,{action:'read'})).text,/Readable source content/);
    await assert.rejects(browser.execute(id,{action:'navigate',url:'https://docs.example/verification'}),/page_verification_required/);
    await assert.rejects(browser.execute(id,{action:'navigate',url:'https://docs.example/denied'}),/page_access_denied/);
    await assert.rejects(browser.execute(id,{action:'navigate',url:'https://docs.example/rate-limited'}),/page_rate_limited/);
    assert.equal(browser.sessions.size,1,'verification and HTTP errors must leave the browser recoverable');
  } finally {await browser.close();}
});

test('real sandboxed CDP: dynamic DOM, type/click/scroll/screenshot, reuse, expiry and SSRF', {skip:process.env.MNEME_BROWSER_TEST!=='1',timeout:60000}, async()=>{
  let privateReached=0,jsonRequests=0;
  const server=http.createServer((req,res)=>{
    if(req.url==='/slow'){res.writeHead(200,{'content-type':'text/html; charset=utf-8'});res.write(`<html><head><title>Streaming page</title></head><body><article><p>${'正文已经加载，可以读取。'.repeat(20)}</p></article>`);return;}
    if(req.url==='/private'){privateReached++;return res.end('pod-secret');}
    if(req.url==='/json'){
      jsonRequests++;res.setHeader('content-type','application/json');return res.end(JSON.stringify({text:'公开 JSON 初始化完成'}));
    }
    if(req.url==='/redirect'){res.writeHead(302,{location:`http://127.0.0.1:${server.address().port}/private`});return res.end();}
    res.setHeader('content-type','text/html; charset=utf-8');
    res.end(`<html><head><title>CDP fixture</title></head><body><article><h1>动态浏览测试</h1><input name="keyword" aria-label="关键词"><button onclick="document.getElementById('result').textContent='动态结果：'+document.querySelector('input').value">运行查询</button><p id="result">等待输入</p><p>${'正文内容。'.repeat(80)}</p><a href="http://fixture.example/next">下一页</a><img src="http://127.0.0.1:${server.address().port}/private"><div style="height:1800px">可滚动正文</div></article></body></html>`);
  });
  await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
  // Trusted in-process test injection only. Production has no private-network allowlist or test env switch.
  class FixturePolicy extends NetworkPolicy {async resolve(input){const url=new URL(input);if(url.hostname==='fixture.example')return{url,host:url.hostname,port:server.address().port,address:'127.0.0.1',family:4};return super.resolve(input);}}
  const browser=new BrowserTools({policy:new FixturePolicy(),maxSessions:1,ttlMs:30});
  const session='11111111-1111-4111-8111-111111111111';
  try {
    await browser.start();
    const diagnostic=await browser.browser.newContext();
    const sandboxPage=await diagnostic.newPage();await sandboxPage.goto('chrome://sandbox');
    const sandboxStatus=await sandboxPage.locator('body').innerText();
    assert.match(sandboxStatus,/Layer 1 Sandbox\s+Namespace/);assert.match(sandboxStatus,/Seccomp-BPF sandbox\s+Yes/);
    await diagnostic.close();
    await browser.session(session);
    await assert.rejects(browser.execute(session,{action:'read'}),/browser_session_missing_navigate/);
    await browser.closeSession(session);
    const opened=await browser.execute(session,{action:'navigate',url:'http://fixture.example/'});
    assert.match(opened.text,/动态浏览测试/);assert.equal(browser.sandbox,true);assert.equal(await browser.sessions.get(session).page.evaluate(()=>typeof RTCPeerConnection),'undefined');
    await assert.rejects(browser.execute('22222222-2222-4222-8222-222222222222',{action:'navigate',url:'http://fixture.example/'}),/browser_capacity/);
    let input=opened.elements.find(e=>e.label==='关键词');assert(input);
    const typed=await browser.execute(session,{action:'type',element:input.ref,text:'浏览器真实输入'});
    const button=typed.elements.find(e=>e.label==='运行查询');assert(button);
    const clicked=await browser.execute(session,{action:'click',element:button.ref});assert.match(clicked.text,/动态结果：浏览器真实输入/);
    const page=browser.sessions.get(session).page;
    assert.equal(await page.evaluate(async()=>{const r=await fetch('/json',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({query:'public page'})});return(await r.json()).text;}),'公开 JSON 初始化完成');
    assert.equal(jsonRequests,1);
    for(const input of [{method:'POST',headers:{'Content-Type':'text/plain'},body:'text'},{method:'DELETE'},{method:'POST',headers:{'Content-Type':'application/json',Authorization:'Bearer fixture'},body:'{}'},{method:'POST',headers:{'Content-Type':'application/json'},body:'invalid json'},{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({data:'x'.repeat(65536)})}]){
      assert.equal(await page.evaluate(async input=>{try{await fetch('/json',input);return'allowed';}catch{return'blocked';}},input),'blocked');
    }
    assert.equal(jsonRequests,1,'unsupported, authenticated and oversized writes must not reach the server');
    assert.equal(await page.evaluate(async url=>{try{await fetch(url,{method:'POST',headers:{'Content-Type':'application/json'},body:'{}'});return'allowed';}catch{return'blocked';}},`http://127.0.0.1:${server.address().port}/private`),'blocked');
    await browser.execute(session,{action:'scroll',delta_y:600});
    const screenshot=await browser.execute(session,{action:'screenshot'});assert.equal(screenshot.image.mime,'image/jpeg');assert(screenshot.image.data.length>1000);
    const live=browser.sessions.get(session);live.page.setDefaultTimeout(50);live.refs.set('e999',live.page.locator('#missing-element'));
    await assert.rejects(browser.execute(session,{action:'click',element:'e999'}),/browser_timeout/);
    assert.equal(browser.sessions.get(session),live,'an ordinary action timeout must keep the page');
    live.page.setDefaultTimeout(8000);assert.match((await browser.execute(session,{action:'read'})).text,/动态结果/);
    assert.equal(privateReached,0,'subresources must not reach internal services');
    await assert.rejects(browser.execute(session,{action:'navigate',url:'http://fixture.example/redirect'}));assert.equal(privateReached,0,'redirect must be blocked');
    await browser.execute(session,{action:'navigate',url:'http://fixture.example/'});
    await new Promise(resolve=>setTimeout(resolve,50));await browser.reap();assert.equal(browser.sessions.size,0);
    await assert.rejects(browser.execute(session,{action:'read'}),/browser_session_missing_navigate/);
    browser.timeoutMs=3000;const partial=await browser.execute(session,{action:'navigate',url:'http://fixture.example/slow'});
    assert.equal(partial.load_incomplete,true);assert.match(partial.text,/正文已经加载/);assert.equal(browser.sessions.size,1);
    assert.match((await browser.execute(session,{action:'read'})).text,/正文已经加载/);
    const snapshot=browser.snapshot;browser.snapshot=()=>new Promise(()=>{});browser.timeoutMs=100;
    await assert.rejects(browser.execute(session,{action:'wait',wait_ms:0}),/tool_timeout/);assert.equal(browser.sessions.size,0);
    browser.snapshot=snapshot;await assert.rejects(browser.execute(session,{action:'read'}),/browser_session_missing_navigate/);
    const child=browser.process;
    child.kill=()=>{throw Object.assign(new Error('fixture: signals denied'),{code:'EACCES'});};
    await browser.close();assert(child.exitCode!==null || child.signalCode!==null,'CDP must terminate Chromium without signal privileges');
  }catch(error){console.error(error.stack);throw error;}
  finally{server.closeAllConnections();await browser.close();await new Promise(resolve=>server.close(resolve));}
});
