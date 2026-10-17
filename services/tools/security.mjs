import dns from 'node:dns/promises';
import http from 'node:http';
import net from 'node:net';
import ipaddr from 'ipaddr.js';

export class ToolError extends Error {
  constructor(code, status = 400) { super(code); this.code = code; this.status = status; }
}
export function publicAddress(address) {
  try { return ipaddr.process(address).range() === 'unicast'; } catch { return false; }
}
export class NetworkPolicy {
  constructor(lookup = dns.lookup) { this.lookup = lookup; }
  async resolve(input) {
    let url;
    try { url = new URL(input); } catch { throw new ToolError('invalid_url'); }
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.href.length > 4096) throw new ToolError('blocked_url');
    const host = url.hostname.replace(/^\[|\]$/g, '').toLowerCase().replace(/\.$/, '');
    const port = Number(url.port || (url.protocol === 'https:' ? 443 : 80));
    if (![80, 443].includes(port) || /(^|\.)(localhost|local|internal|lan|home|test|invalid)$/.test(host)) throw new ToolError('blocked_destination');
    const records = net.isIP(host) ? [{ address: host, family: net.isIP(host) }] : await this.lookup(host, { all: true, verbatim: true }).catch(() => { throw new ToolError('dns_failed', 502); });
    if (!records.length || records.some(r => !publicAddress(r.address))) throw new ToolError('blocked_destination');
    return { url, host, port, ...records[0] };
  }
}

// Every connection resolves once, validates ALL answers and connects to the chosen IP.
// Chromium's proxy bypass, QUIC, non-proxied WebRTC and WebSockets are disabled separately.
export async function startEgressProxy(policy) {
  const sockets = new Set();
  const server = http.createServer(async (request, response) => {
    try {
      const target = await policy.resolve(request.url);
      if (target.url.protocol !== 'http:' || request.headers.upgrade) throw new ToolError('blocked_protocol');
      const headers = { ...request.headers, host: target.url.host, connection: 'close' };
      for (const key of ['proxy-authorization', 'proxy-connection', 'forwarded', 'x-forwarded-for']) delete headers[key];
      const upstream = http.request({ host: target.address, family: target.family, port: target.port, method: request.method, path: target.url.pathname + target.url.search, headers, timeout: 25000 }, incoming => {
        response.writeHead(incoming.statusCode, incoming.headers);
        let size = 0;
        incoming.on('data', chunk => { size += chunk.length; if (size > 16 * 1024 * 1024) { upstream.destroy(); response.destroy(); } });
        incoming.pipe(response);
      });
      upstream.on('timeout', () => upstream.destroy());
      upstream.on('error', () => { if (!response.headersSent) response.writeHead(502); response.end(); });
      request.on('aborted', () => upstream.destroy());
      request.pipe(upstream);
    } catch { response.writeHead(403); response.end('blocked_destination'); }
  });
  server.on('connect', async (request, downstream, head) => {
    try {
      if (!/^[\w.\-[\]:]+$/.test(request.url) || !request.url.endsWith(':443')) throw new ToolError('blocked_destination');
      const target = await policy.resolve(`https://${request.url}`);
      const upstream = net.connect({ host: target.address, family: target.family, port: target.port });
      let transferred = 0;
      for (const socket of [upstream, downstream]) {
        socket.setTimeout(30000, () => socket.destroy());
        socket.on('data', chunk => { transferred += chunk.length; if (transferred > 32 * 1024 * 1024) { upstream.destroy(); downstream.destroy(); } });
        socket.on('error', () => { upstream.destroy(); downstream.destroy(); });
      }
      upstream.once('connect', () => { downstream.write('HTTP/1.1 200 Connection Established\r\n\r\n'); if (head.length) upstream.write(head); downstream.pipe(upstream); upstream.pipe(downstream); });
      downstream.once('close', () => upstream.destroy());
      upstream.once('close', () => downstream.destroy());
    } catch { downstream.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n'); }
  });
  server.on('connection', socket => { if (sockets.size >= 128) { socket.destroy(); return; } sockets.add(socket); socket.on('close', () => sockets.delete(socket)); socket.setTimeout(30000, () => socket.destroy()); });
  server.on('clientError', (_, socket) => socket.destroy());
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  return { port: server.address().port, close: async () => { for (const socket of sockets) socket.destroy(); await new Promise(resolve => server.close(resolve)); } };
}
