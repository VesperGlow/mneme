import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { BrowserTools } from '../browser.mjs';
import { NetworkPolicy, ToolError } from '../security.mjs';

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

test('real sandboxed CDP: repeat search submissions, Google/Bing/DDG results and verification errors', {skip:process.env.MNEME_BROWSER_TEST!=='1',timeout:60000}, async()=>{
  class FixturePolicy extends NetworkPolicy {
    async resolve(input) {
      const url=new URL(input);
      if(['www.google.com','www.bing.com','html.duckduckgo.com'].includes(url.hostname)) return {url,host:url.hostname,port:443,address:'93.184.216.34',family:4};
      return super.resolve(input);
    }
  }
  const browser=new BrowserTools({policy:new FixturePolicy()});
  const id='11111111-1111-4111-8111-111111111111';
  try {
    assert.equal(browser.engine,'google');
    const session=await browser.session(id);
    const queries=[];
    await session.context.route(/^https:\/\/(www\.google\.com|www\.bing\.com|html\.duckduckgo\.com)\//,async route=>{
      const url=new URL(route.request().url());
      let body;
      if(url.pathname==='/sorry/index') body='<title>Verification</title><p>Our systems have detected unusual traffic from your computer network.</p>';
      else if(url.pathname==='/denied') return route.fulfill({status:403,contentType:'text/html',body:'<title>Access denied</title><p>Access denied</p>'});
      else if(url.searchParams.has('q')) {
        const query=url.searchParams.get('q');queries.push(query);
        if(query==='verification') return route.fulfill({contentType:'text/html',body:'<title>Verification</title><p>Our systems have detected unusual traffic from your computer network.</p>'});
        body=url.hostname==='www.google.com'
          ? '<div id="search"><div class="MjjYud"><a href="https://www.debian.org/"><span>www.debian.org</span><h3>Debian official site</h3></a><p>The universal operating system. Documentation about unusual traffic.</p></div><div class="MjjYud"><a href="/url?q=https%3A%2F%2Fwww.rust-lang.org%2F"><h3>Rust official site</h3></a><p>A language for reliable software.</p></div></div>'
          : url.hostname==='www.bing.com'
            ? '<li class="b_algo"><h2><a href="https://www.debian.org/">Debian official site</a></h2><p>The universal operating system.</p></li>'
            : '<div class="result"><a class="result__a" href="https://www.debian.org/">Debian official site</a><p>The universal operating system.</p></div>';
      } else {
        // Model the autocomplete handler that swallowed Enter in production.
        const submit=url.hostname==='www.google.com'?'name="btnK"':url.hostname==='www.bing.com'?'id="sb_form_go"':'';
        body=`<form action="/search" onkeydown="if(event.key==='Enter')event.preventDefault()"><input name="q"><input type="submit" ${submit} value="Search"></form>`;
      }
      await route.fulfill({contentType:'text/html; charset=utf-8',body:`<html><body>${body}</body></html>`});
    });
    for(const engine of ['google','bing','duckduckgo']) {
      browser.engine=engine;
      for(const query of ['Debian official website','Rust programming language']) {
        const result=await browser.execute(id,{action:'search',query});
        assert.equal(result.results[0].title,'Debian official site');
        assert.equal(result.results[0].url,'https://www.debian.org/');
        assert.match(result.results[0].snippet,/universal operating system/);
        if(engine==='google') {
          assert.equal(result.results.length,2);
          assert.equal(result.results[1].url,'https://www.rust-lang.org/');
        }
      }
    }
    assert.equal(queries.length,6,'each search must submit exactly once');
    browser.engine='google';
    await assert.rejects(browser.execute(id,{action:'search',query:'verification'}),/search_verification_required/);
    await assert.rejects(browser.execute(id,{action:'read'}),/page_verification_required/);
    await assert.rejects(browser.execute(id,{action:'navigate',url:'https://www.google.com/denied'}),/page_access_denied/);
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
    await assert.rejects(browser.execute(session,{action:'read'}),/browser_session_missing_navigate_or_search/);
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
    await assert.rejects(browser.execute(session,{action:'read'}),/browser_session_missing_navigate_or_search/);
    browser.timeoutMs=3000;const partial=await browser.execute(session,{action:'navigate',url:'http://fixture.example/slow'});
    assert.equal(partial.load_incomplete,true);assert.match(partial.text,/正文已经加载/);assert.equal(browser.sessions.size,1);
    assert.match((await browser.execute(session,{action:'read'})).text,/正文已经加载/);
    const snapshot=browser.snapshot;browser.snapshot=()=>new Promise(()=>{});browser.timeoutMs=100;
    await assert.rejects(browser.execute(session,{action:'wait',wait_ms:0}),/tool_timeout/);assert.equal(browser.sessions.size,0);
    browser.snapshot=snapshot;await assert.rejects(browser.execute(session,{action:'read'}),/browser_session_missing_navigate_or_search/);
    const child=browser.process;
    child.kill=()=>{throw Object.assign(new Error('fixture: signals denied'),{code:'EACCES'});};
    await browser.close();assert(child.exitCode!==null || child.signalCode!==null,'CDP must terminate Chromium without signal privileges');
  }catch(error){console.error(error.stack);throw error;}
  finally{server.closeAllConnections();await browser.close();await new Promise(resolve=>server.close(resolve));}
});
