import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import net from 'node:net';
import { NetworkPolicy, publicAddress, startEgressProxy } from '../security.mjs';
import { createServer } from '../app.mjs';
const token = 'tools-test-token-01234567890123456789';

test('SSRF policy rejects special addresses, mapped IPs, DNS rebinding and credentials', async () => {
  for (const address of ['0.0.0.0','127.0.0.1','10.1.2.3','172.20.0.1','192.168.1.1','169.254.169.254','100.64.0.1','198.18.0.1','224.0.0.1','240.0.0.1','::','::1','fe80::1','fc00::1','::ffff:127.0.0.1','2002:7f00:1::']) assert.equal(publicAddress(address), false, address);
  const policy = new NetworkPolicy(async () => [{ address: '93.184.216.34', family: 4 }, { address: '127.0.0.1', family: 4 }]);
  for (const url of ['file:///etc/passwd','http://localhost/','http://0x7f000001/','http://2130706433/','http://[::ffff:127.0.0.1]/','http://169.254.169.254/latest/meta-data','https://user:secret@example.com/','http://public.example:8080/','http://rebinding.example/']) await assert.rejects(policy.resolve(url));
  let count = 0;
  const rebinding = new NetworkPolicy(async () => [{ address: ++count === 1 ? '93.184.216.34' : '10.0.0.1', family: 4 }]);
  assert.equal((await rebinding.resolve('https://public.example')).address, '93.184.216.34');
  await assert.rejects(rebinding.resolve('https://public.example'));
  assert.equal(publicAddress('93.184.216.34'), true);
});

test('egress proxy blocks direct HTTP and CONNECT to the Pod loopback', async () => {
  let reached = 0;
  const trap = http.createServer((_, res) => { reached++; res.end('secret'); });
  await new Promise(resolve => trap.listen(0, '127.0.0.1', resolve));
  const proxy = await startEgressProxy(new NetworkPolicy());
  try {
    const status = await new Promise((resolve, reject) => { const request = http.request({host:'127.0.0.1',port:proxy.port,path:`http://127.0.0.1:${trap.address().port}/`},r=>{r.resume();resolve(r.statusCode);});request.on('error',reject);request.end(); });
    assert.equal(status,403);
    const response = await new Promise(resolve => {const socket=net.connect(proxy.port,'127.0.0.1',()=>socket.write('CONNECT 127.0.0.1:443 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n'));socket.once('data',d=>{resolve(d.toString());socket.destroy();});});
    assert.match(response,/403/);assert.equal(reached,0);
  } finally {await proxy.close();await new Promise(resolve=>trap.close(resolve));}
});

test('internal API requires independent credentials and validates every action', async () => {
  const fake = {sessions:new Map(),browser:{isConnected:()=>true},maxSessions:4,ttlMs:600000,sandbox:true,engine:'bing',execute:async()=>({url:'https://example.org',text:'fixture'}),close:async()=>{},reset:async()=>{},closeSession:async()=>{}};
  const app=await createServer({token,browser:fake,eager:false});
  try {
    assert.equal((await app.inject({url:'/health'})).statusCode,401);
    assert.equal((await app.inject({url:'/health',headers:{authorization:token}})).statusCode,401);
    const headers={authorization:`Bearer ${token}`};
    assert.equal((await app.inject({url:'/health',headers})).statusCode,200);
    assert.equal((await app.inject({url:'/health',headers:{...headers,'sec-fetch-site':'cross-site'}})).statusCode,403);
    for(const args of [{action:'evaluate',script:'steal()'},{action:'navigate'},{action:'type',element:'e1'},{action:'wait',wait_ms:99999},{action:'read',unexpected:true}]) {
      const result=await app.inject({method:'POST',url:'/v1/execute',headers,payload:{session_id:'11111111-1111-4111-8111-111111111111',arguments:args}});
      assert.equal(result.statusCode,400,result.body);
    }
    const invalid=await app.inject({method:'POST',url:'/v1/images/normalize',headers:{...headers,'content-type':'application/octet-stream'},payload:Buffer.from('<svg xmlns="http://www.w3.org/2000/svg"/>')});
    assert.equal(invalid.statusCode,400);
  } finally {await app.close();}
});
