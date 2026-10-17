import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import { BrowserTools } from '../browser.mjs';
import { NetworkPolicy, ToolError } from '../security.mjs';

test('real sandboxed CDP: dynamic DOM, type/click/scroll/screenshot, reuse, expiry and SSRF', {skip:process.env.MNEME_BROWSER_TEST!=='1',timeout:60000}, async()=>{
  let privateReached=0;
  const server=http.createServer((req,res)=>{
    if(req.url==='/slow'){res.writeHead(200,{'content-type':'text/html'});res.write('<html>');return;}
    if(req.url==='/private'){privateReached++;return res.end('pod-secret');}
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
    const opened=await browser.execute(session,{action:'navigate',url:'http://fixture.example/'});
    assert.match(opened.text,/动态浏览测试/);assert.equal(browser.sandbox,true);assert.equal(await browser.sessions.get(session).page.evaluate(()=>typeof RTCPeerConnection),'undefined');
    await assert.rejects(browser.execute('22222222-2222-4222-8222-222222222222',{action:'read'}),/browser_capacity/);
    let input=opened.elements.find(e=>e.label==='关键词');assert(input);
    const typed=await browser.execute(session,{action:'type',element:input.ref,text:'浏览器真实输入'});
    const button=typed.elements.find(e=>e.label==='运行查询');assert(button);
    const clicked=await browser.execute(session,{action:'click',element:button.ref});assert.match(clicked.text,/动态结果：浏览器真实输入/);
    await browser.execute(session,{action:'scroll',delta_y:600});
    const screenshot=await browser.execute(session,{action:'screenshot'});assert.equal(screenshot.image.mime,'image/jpeg');assert(screenshot.image.data.length>1000);
    assert.equal(privateReached,0,'subresources must not reach internal services');
    await assert.rejects(browser.execute(session,{action:'navigate',url:'http://fixture.example/redirect'}));assert.equal(privateReached,0,'redirect must be blocked');
    await browser.execute(session,{action:'navigate',url:'http://fixture.example/'});
    await new Promise(resolve=>setTimeout(resolve,50));await browser.reap();assert.equal(browser.sessions.size,0);
    browser.timeoutMs=3000;await assert.rejects(browser.execute(session,{action:'navigate',url:'http://fixture.example/slow'}),/timeout/);assert.equal(browser.sessions.size,0);
  }finally{await browser.close();await new Promise(resolve=>server.close(resolve));}
});
