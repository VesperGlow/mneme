import { test } from 'node:test';
import assert from 'node:assert/strict';
import { TavilySearch } from '../search.mjs';
import { BrowserTools } from '../browser.mjs';
import { createServer } from '../app.mjs';

const result = { title: 'Debian', url: 'https://www.debian.org/', content: 'The universal operating system.' };
const response = () => Response.json({ results: [result] });

test('single Tavily key uses the official search protocol and returns bounded source snippets', async () => {
  const search = new TavilySearch({ apiKeys: 'tvly-single', fetchImpl: async (url, options) => {
    assert.equal(url, 'https://api.tavily.com/search');
    assert.equal(options.method, 'POST');
    assert.equal(options.redirect, 'error');
    assert.equal(options.headers.Authorization, 'Bearer tvly-single');
    assert.equal(options.headers['Content-Type'], 'application/json');
    assert.equal(JSON.parse(options.body).query, 'Debian 官网');
    assert.equal(JSON.parse(options.body).search_depth, 'basic');
    assert.equal(JSON.parse(options.body).max_results, 10);
    return Response.json({ results: [result, { title: 'long'.repeat(200), url: 'https://example.org/', content: 'x'.repeat(10000) }, null, { url: 'file:///etc/passwd' }, { url: 'https://secret@example.org' }] });
  } });
  for (let i = 0; i < 2; i++) {
    const value = await search.execute(' Debian 官网 ');
    assert.equal(value.provider, 'tavily');
    assert.equal(value.untrusted, true);
    assert.deepEqual(value.results[0], { title: result.title, url: result.url, snippet: result.content });
    assert.deepEqual(value.links[0], { title: result.title, url: result.url });
    assert.equal(value.results.length, 2);
    assert.equal(value.results[1].title.length, 180);
    assert.equal(value.results[1].snippet.length, 1200);
    assert.match(value.text, /universal operating system/);
    assert(!JSON.stringify(value).includes('tvly-single'));
  }
});

test('HTTP searches rotate keys globally across concurrent sessions without starting Chromium', async () => {
  const used = [];
  const search = new TavilySearch({ apiKeys: ' / tvly-first // tvly-second / tvly-third / ', fetchImpl: async (_, options) => {
    used.push(options.headers.Authorization);
    await new Promise(resolve => setImmediate(resolve));
    return response();
  } });
  const browser = new BrowserTools({ search });
  browser.start = async () => { throw new Error('search must not start Chromium'); };
  const token = 'tools-search-fixture-01234567890123456789';
  const app = await createServer({ token, browser, eager: false });
  const headers = { authorization: `Bearer ${token}` };
  try {
    const calls = await Promise.all(Array.from({ length: 7 }, (_, i) => app.inject({ method: 'POST', url: '/v1/execute', headers, payload: {
      session_id: `11111111-1111-4111-8111-${String(i).padStart(12, '0')}`, arguments: { action: 'search', query: `query ${i}` },
    } })));
    // The service bounds heavy calls to four; rejected calls must not consume a key.
    assert.equal(calls.filter(call => call.statusCode === 200).length, 4);
    assert.equal(calls.filter(call => call.statusCode === 429).length, 3);
    for (let i = 0; i < 3; i++) await browser.execute('another-session', { action: 'search', query: 'next query' });
    assert.deepEqual(used, ['first', 'second', 'third', 'first', 'second', 'third', 'first'].map(key => `Bearer tvly-${key}`));
    assert.equal(browser.sessions.size, 0);
    await assert.rejects(browser.execute('another-session', { action: 'read' }), /browser_session_missing_navigate/);
    const health = (await app.inject({ url: '/health', headers })).json();
    assert.equal(health.engine, 'tavily');
    assert.equal(health.search_configured, true);
    assert(!JSON.stringify(health).includes('tvly-first'));
  } finally { await app.close(); }
});

test('missing keys and invalid queries fail before any request', async () => {
  const search = new TavilySearch({ apiKeys: '///', fetchImpl: () => { throw new Error('must not fetch'); } });
  assert.equal(search.configured, false);
  await assert.rejects(search.execute('Debian'), /tavily_api_key_required/);
  for (const query of ['', '   ', null, 'x'.repeat(501)]) await assert.rejects(search.execute(query), /invalid_search_query/);
});

test('upstream failures use safe codes and the following search advances to the next key', async () => {
  for (const [status, code] of [[401, 'tavily_auth_failed'], [403, 'tavily_auth_failed'], [429, 'tavily_rate_limited'], [432, 'tavily_quota_exceeded'], [433, 'tavily_quota_exceeded'], [500, 'tavily_search_failed']]) {
    const used = [];
    const search = new TavilySearch({ apiKeys: 'tvly-bad/tvly-good', fetchImpl: async (_, options) => {
      used.push(options.headers.Authorization);
      return used.length === 1 ? new Response('private upstream error tvly-bad', { status }) : response();
    } });
    await assert.rejects(search.execute('Debian'), error => error.code === code && error.status === 502 && !error.message.includes('tvly-bad'));
    assert.equal((await search.execute('Debian')).results.length, 1);
    assert.deepEqual(used, ['Bearer tvly-bad', 'Bearer tvly-good']);
  }
});

test('malformed, oversized and interrupted responses are bounded and sanitized', async () => {
  for (const [fetchImpl, code] of [
    [async () => new Response('{invalid JSON'), 'tavily_invalid_response'],
    [async () => Response.json({ detail: 'tvly-secret' }), 'tavily_invalid_response'],
    [async () => new Response('x'.repeat(1024 * 1024 + 1)), 'tavily_response_too_large'],
    [async () => { throw new Error('transport failure tvly-secret'); }, 'tavily_search_failed'],
  ]) {
    const search = new TavilySearch({ apiKeys: 'tvly-secret', fetchImpl });
    await assert.rejects(search.execute('Debian'), error => error.code === code && !error.message.includes('tvly-secret'));
  }
  const empty = new TavilySearch({ apiKeys: 'tvly-secret', fetchImpl: async () => Response.json({ results: [] }) });
  assert.deepEqual((await empty.execute('no matches')).results, []);
});

test('search deadline cancels both request and response streaming', async () => {
  for (const streaming of [false, true]) {
    const search = new TavilySearch({ apiKeys: 'tvly-test', timeoutMs: 20, fetchImpl: async (_, { signal }) => {
      if (streaming) return new Response(new ReadableStream({ start(controller) { signal.addEventListener('abort', () => controller.error(signal.reason), { once: true }); } }));
      return new Promise((_, reject) => signal.addEventListener('abort', () => reject(signal.reason), { once: true }));
    } });
    // AbortSignal.timeout is unref'ed; keep the mocked transport alive like a real socket.
    const keepAlive = setInterval(() => {}, 1000);
    try { await assert.rejects(search.execute('Debian'), error => error.code === 'tavily_timeout' && error.status === 504); }
    finally { clearInterval(keepAlive); }
  }
});
