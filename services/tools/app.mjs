import Fastify from 'fastify';
import { timingSafeEqual } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import sharp from 'sharp';
import { BrowserTools } from './browser.mjs';
import { ToolError } from './security.mjs';

sharp.concurrency(1);
sharp.cache({ memory: 16, files: 0, items: 0 });
const catalog = JSON.parse(await readFile(new URL('./catalog.json', import.meta.url), 'utf8'));
export const browserParameters = catalog.tools[0].function.parameters;
const uuid = { type: 'string', pattern: '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$' };
export async function createServer({ token, browser = new BrowserTools(), eager = true } = {}) {
  if (!token || token.length < 32 || /\s/.test(token)) throw new Error('tools_token_required_minimum_32_characters');
  const app = Fastify({ logger: false, bodyLimit: 32768, requestTimeout: 35000, connectionTimeout: 35000, ajv: { customOptions: { removeAdditional: false, coerceTypes: false } } });
  app.decorateRequest('counted', false);
  const metrics = { calls: 0, failures: 0, total_ms: 0 };
  let active = 0;
  app.addContentTypeParser('application/octet-stream', { parseAs: 'buffer', bodyLimit: 8 * 1024 * 1024 }, (_, body, done) => done(null, body));
  app.addHook('onRequest', async (request, reply) => {
    reply.header('cache-control', 'no-store').header('x-content-type-options', 'nosniff');
    const authorization = request.headers.authorization || '';
    const supplied = Buffer.from(authorization.startsWith('Bearer ') ? authorization.slice(7) : '');
    const expected = Buffer.from(token);
    if (supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) return reply.code(401).send({ error: 'unauthorized' });
    if (request.headers['sec-fetch-site'] === 'cross-site') return reply.code(403).send({ error: 'cross_site_request' });
    if (['/v1/execute','/v1/images/normalize'].includes(request.url)) {
      if (active >= 4) throw new ToolError('tools_busy', 429);
      active++; request.counted = true;
    }
  });
  app.addHook('onResponse', async request => { if (request.counted) active--; });
  app.setErrorHandler((error, _, reply) => {
    const code = error instanceof ToolError ? error.code : error.validation ? 'invalid_arguments' : error.statusCode === 413 ? 'body_too_large' : 'tool_internal_error';
    reply.code(error instanceof ToolError ? error.status : error.validation ? 400 : error.statusCode === 413 ? 413 : 500).send({ error: code });
  });
  app.get('/health', async () => ({ ready: Boolean(browser.browser?.isConnected()), active_sessions: browser.sessions.size, max_sessions: browser.maxSessions, idle_timeout_seconds: browser.ttlMs / 1000, sandbox: browser.sandbox, engine: browser.engine, active_calls: active, metrics }));
  app.get('/v1/tools', async () => catalog);
  app.post('/v1/execute', { schema: { body: { type: 'object', additionalProperties: false, required: ['session_id', 'arguments'], properties: { session_id: uuid, arguments: browserParameters } } } }, async (request, reply) => {
    const started = performance.now(); metrics.calls++;
    try {
      const result = await browser.execute(request.body.session_id, request.body.arguments);
      const elapsed_ms = Math.round(performance.now() - started); metrics.total_ms += elapsed_ms;
      console.log(JSON.stringify({ event: 'tool_call', action: request.body.arguments.action, status: 'completed', elapsed_ms }));
      return { result, elapsed_ms };
    } catch (error) {
      metrics.failures++; metrics.total_ms += Math.round(performance.now() - started);
      console.log(JSON.stringify({ event: 'tool_call', action: request.body.arguments.action, status: 'failed', error: error instanceof ToolError ? error.code : 'tool_internal_error', elapsed_ms: Math.round(performance.now() - started) }));
      throw error;
    }
  });
  app.delete('/v1/sessions/:id', { schema: { params: { type: 'object', properties: { id: uuid }, required: ['id'] } } }, async request => { await browser.closeSession(request.params.id); return { closed: true }; });
  app.post('/v1/reset', async () => { await browser.reset(); return { reset: true }; });
  app.post('/v1/images/normalize', { bodyLimit: 8 * 1024 * 1024 }, async request => {
    if (!Buffer.isBuffer(request.body) || request.body.length === 0) throw new ToolError('invalid_image');
    try {
      const image = sharp(request.body, { limitInputPixels: 20_000_000, animated: false, failOn: 'warning' });
      const metadata = await image.metadata();
      if (!['png', 'jpeg', 'webp'].includes(metadata.format)) throw new ToolError('unsupported_image');
      const { data, info } = await image.rotate().resize({ width: 2048, height: 2048, fit: 'inside', withoutEnlargement: true }).flatten({ background: '#ffffff' }).jpeg({ quality: 82 }).toBuffer({ resolveWithObject: true });
      if (data.length > 2 * 1024 * 1024) throw new ToolError('image_too_large');
      return { mime: 'image/jpeg', data: data.toString('base64'), width: info.width, height: info.height };
    } catch (error) { if (error instanceof ToolError) throw error; throw new ToolError('invalid_image'); }
  });
  app.addHook('onClose', async () => browser.close());
  if (eager) await browser.start();
  return app;
}

if (process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href) {
  const browser = new BrowserTools({ executablePath: process.env.MNEME_CHROMIUM_PATH || '/usr/bin/chromium', engine: process.env.MNEME_SEARCH_ENGINE || 'bing' });
  const app = await createServer({ token: process.env.MNEME_TOOLS_TOKEN, browser });
  await app.listen({ host: '127.0.0.1', port: Number(process.env.MNEME_TOOLS_PORT || 8091) });
  console.log(JSON.stringify({ event: 'tools_started', transport: 'loopback_http', sandbox: browser.sandbox }));
  for (const signal of ['SIGTERM', 'SIGINT']) process.once(signal, () => app.close().then(() => process.exit(0)));
}
