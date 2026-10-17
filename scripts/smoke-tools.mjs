// Real Chromium/CDP and Rust orchestration with a deterministic, local LLM protocol fixture.
// No search API, real LLM credentials, production ports/data or vision model is used.
import assert from 'node:assert/strict';
import http from 'node:http';
import { spawn } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { createServer } from '../services/tools/app.mjs';
import { BrowserTools } from '../services/tools/browser.mjs';
const require=createRequire(new URL('../services/tools/package.json',import.meta.url));
const sharp=require('sharp');
const {chromium}=require('playwright-core');
const token='console-test-token-01234567890123456789';
const toolsToken='tools-qa-only-token-01234567890123456789';
const BASE='http://127.0.0.1:18080';
const headers={Authorization:`Bearer ${token}`,'Content-Type':'application/json'};
const streamRequests=[];
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));
let child,ui;
const engine=process.env.MNEME_SEARCH_ENGINE||'google';
const browser=new BrowserTools({engine});
const tools=await createServer({token:toolsToken,browser});
await tools.listen({host:'127.0.0.1',port:0});
const llm=http.createServer(async(req,res)=>{
  try{
    const chunks=[];for await(const chunk of req)chunks.push(chunk);const body=JSON.parse(Buffer.concat(chunks));
    assert.equal(req.headers.authorization,'Bearer tools-qa-llm-fixture-only');
    if(!body.stream){res.setHeader('Content-Type','application/json');return res.end(JSON.stringify({choices:[{message:{content:'{"memories":[]}'},finish_reason:'stop'}]}));}
    streamRequests.push(body);
    const history=body.messages.filter(m=>m.role==='tool');
    const current=[...body.messages].reverse().find(m=>m.role==='user'&&m.name!=='tool_images');
    const content=typeof current.content==='string'?current.content:current.content.filter(p=>p.type==='text').map(p=>p.text).join('\n');
    const imageOnly=content.includes('识别图片');const continuing=content.includes('继续滚动');const blocked=content.includes('内部地址');
    let action,args,source;
    if(body.tools && !history.length){
      if(imageOnly){action='view_image';args={asset_id:content.match(/[0-9a-f]{8}-[0-9a-f-]{27}/)?.[0]};}
      else{action='browser';args=continuing?{action:'scroll',delta_y:500}:blocked?{action:'navigate',url:'http://127.0.0.1:7474/'}:{action:'search',query:'Debian Linux official website'};}
    }else if(body.tools && !imageOnly && !blocked){
      if(!continuing && history.length===1){
        const result=JSON.parse(history[0].content);
        source=result.results?.find(r=>new URL(r.url).hostname.endsWith('debian.org'))?.url;
        assert(source,'real browser search must return an actual Debian source link');action='browser';args={action:'navigate',url:source};
      }else if(!continuing&&history.length===2){action='browser';args={action:'screenshot'};}
      else if((!continuing&&history.length===3)||(continuing&&history.length===1)){action='browser';args={action:'read'};}
    }
    res.writeHead(200,{'Content-Type':'text/event-stream'});
    if(action){
      const argumentsText=JSON.stringify(args);const cut=Math.floor(argumentsText.length/2);
      const frames=[{choices:[{delta:{tool_calls:[{index:0,id:`qa_${history.length}`,type:'function',function:{name:action,arguments:argumentsText.slice(0,cut)}}]},finish_reason:null}]},{choices:[{delta:{tool_calls:[{index:0,function:{arguments:argumentsText.slice(cut)}}]},finish_reason:'tool_calls'}]}];
      for(const frame of frames)res.write(`data: ${JSON.stringify(frame)}\n\n`);
    }else{
      const text=blocked?'已阻止访问 Pod 内部服务。':imageOnly?'已收到上传图片，并通过当前多模态消息协议查看。':continuing?'已继续滚动并阅读同一个浏览器会话。':'已通过真实浏览器搜索并阅读 Debian 官网，来源：https://www.debian.org/';
      res.write(`data: ${JSON.stringify({choices:[{delta:{content:text},finish_reason:'stop'}]})}\n\n`);
    }
    res.end('data: [DONE]\n\n');
  }catch(error){res.statusCode=500;res.end(JSON.stringify({error:error.message}));console.error(error.message);}
});
await new Promise(resolve=>llm.listen(0,'127.0.0.1',resolve));
async function call(path,body,method='POST'){
 const response=await fetch(BASE+path,{method,headers,body:body===undefined?undefined:JSON.stringify(body)});
 const result=await response.json();assert(response.ok,JSON.stringify(result));return result;
}
async function chat(session,message,assets=[]){
 const response=await fetch(`${BASE}/v1/sessions/${session}/chat`,{method:'POST',headers,body:JSON.stringify({request_id:crypto.randomUUID(),message,assets})});
 assert.equal(response.status,200);const wire=await response.text();assert(wire.includes('event: done'),wire);return wire;
}
try{
 child=spawn('cargo',['run','--locked','--example','tools_fixture'],{cwd:process.cwd(),env:{...process.env,MNEME_QA_LLM_URL:`http://127.0.0.1:${llm.address().port}`,MNEME_QA_TOOLS_URL:`http://127.0.0.1:${tools.server.address().port}`},stdio:['ignore','pipe','pipe']});
 let childLog='';child.stdout.on('data',c=>{childLog+=c;});child.stderr.on('data',c=>{childLog+=c;});
 for(let i=0;i<180;i++){if(child.exitCode!==null)throw new Error(childLog);if(await fetch(BASE+'/health/live').then(r=>r.ok).catch(()=>false))break;if(i===179)throw new Error(childLog);await delay(500);}
 const session=(await call('/v1/sessions',{})).id;
 const wire=await chat(session,'请搜索 Debian 官网并阅读、截图。');
 assert(wire.includes('event: tool'));const history=await call(`/v1/sessions/${session}/messages`,undefined,'GET');
 assert.deepEqual(history.tool_calls.map(c=>c.action).reverse(),['search','navigate','screenshot','read']);
 assert(history.tool_calls.every(c=>c.status==='completed'),JSON.stringify(history.tool_calls));
 assert(history.tool_calls.some(c=>c.result.url?.includes('debian.org')));
 const screenshot=history.tool_calls.find(c=>c.action==='screenshot').result.asset_id;
 const shot=await fetch(BASE+`/v1/assets/${screenshot}`,{headers});assert.equal(shot.headers.get('content-type'),'image/jpeg');
 await mkdir('artifacts',{recursive:true});await writeFile('artifacts/tools-browser-live.jpg',Buffer.from(await shot.arrayBuffer()));
 assert(streamRequests.some(r=>r.messages.some(m=>m.name==='tool_images'&&m.content.some(p=>p.type==='image_url'))));
 const before=browser.sessions.get(session).page;
 await chat(session,'继续滚动并阅读');assert.equal(browser.sessions.get(session).page,before,'browser context/page must be reused across turns');
 const jpg=await sharp({create:{width:64,height:48,channels:3,background:'#579166'}}).jpeg().toBuffer();
 const upload=await fetch(BASE+`/v1/sessions/${session}/images`,{method:'POST',headers:{Authorization:`Bearer ${token}`,'Content-Type':'application/octet-stream'},body:jpg});assert.equal(upload.status,201);
 const uploaded=await upload.json();await chat(session,'识别图片',[uploaded.asset_id]);
 assert(streamRequests.some(r=>r.messages.some(m=>m.role==='user'&&Array.isArray(m.content)&&m.content.some(p=>p.image_url?.url.startsWith('data:image/jpeg;base64,')))));
 await chat(session,'尝试访问内部地址');const audit=await call('/v1/console/tools/calls',undefined,'GET');assert.equal(audit.calls[0].error,'blocked_destination');
 // Browser UI uses the same authenticated APIs and displays real tool records and images.
 ui=await chromium.launch({executablePath:'/usr/bin/chromium',headless:true,chromiumSandbox:true,args:['--disable-dev-shm-usage']});
 const page=await ui.newPage({viewport:{width:1440,height:1000}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
 await page.goto(BASE+'/console/');await page.locator('#token').fill(token);await page.getByRole('button',{name:'解锁控制台'}).click();
 await page.getByRole('link',{name:'工具记录',exact:true}).click();await page.getByRole('heading',{name:'最近工具调用'}).waitFor();
 await page.locator('.tool-log').first().locator('summary').first().click();await page.getByText('blocked_destination',{exact:true}).waitFor();await page.screenshot({path:'artifacts/tools-console-desktop.png',fullPage:true});
 await page.getByRole('link',{name:'对话',exact:true}).click();await page.locator('#chat-message').waitFor();
 await page.locator('#chat-message').fill('继续滚动并阅读');await page.getByRole('button',{name:'发送',exact:true}).click();
 await page.locator('#chat-progress').waitFor({state:'hidden',timeout:45000});await page.getByText('已继续滚动并阅读同一个浏览器会话。',{exact:true}).last().waitFor();
 await page.locator('#chat-files').setInputFiles({name:'fixture.jpg',mimeType:'image/jpeg',buffer:jpg});
 await page.locator('#chat-attachments .tag').waitFor();
 await page.locator('#chat-message').fill('识别图片');await page.getByRole('button',{name:'发送',exact:true}).click();
 await page.locator('#chat-progress').waitFor({state:'hidden',timeout:45000});
 await page.locator('.chat-assets').getByRole('button',{name:'查看上传图片',exact:true}).last().click();
 await page.locator('.chat-assets .chat-image').last().waitFor();
 await page.screenshot({path:'artifacts/tools-console-chat.png',fullPage:true});
 const mobile=await ui.newPage({viewport:{width:390,height:844},isMobile:true,hasTouch:true});mobile.on('pageerror',e=>errors.push(e.message));
 await mobile.goto(BASE+'/console/');await mobile.locator('#token').fill(token);await mobile.getByRole('button',{name:'解锁控制台'}).click();await mobile.getByRole('link',{name:'对话',exact:true}).click();await mobile.locator('#chat-message').waitFor();
 assert(await mobile.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'mobile chat overflows');await mobile.screenshot({path:'artifacts/tools-console-mobile.png',fullPage:true});
 assert.deepEqual(errors,[]);
 const report={tested_at:new Date().toISOString(),engine,real_browser:true,cdp:true,sandbox:true,search_api_used:false,source:'https://www.debian.org/',tool_actions:history.tool_calls.map(c=>({action:c.action,status:c.status,elapsed_ms:c.elapsed_ms})).reverse(),context_reused:true,uploaded_image:true,screenshot_image:true,internal_ssrf_blocked:true,desktop_mobile_console:true,llm:'deterministic local protocol fixture; no claim about a real provider vision quality'};
 await writeFile('artifacts/tools-validation.json',JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify(report,null,2));
}finally{
 if(ui)await ui.close();if(child&&child.exitCode===null){child.kill('SIGINT');await Promise.race([new Promise(resolve=>child.once('exit',resolve)),delay(5000)]);if(child.exitCode===null)child.kill('SIGTERM');}
 await tools.close();await new Promise(resolve=>llm.close(resolve));
}
