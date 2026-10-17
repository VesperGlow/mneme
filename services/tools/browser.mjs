import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { chromium } from 'playwright-core';
import { ToolError, NetworkPolicy, startEgressProxy } from './security.mjs';
const require = createRequire(import.meta.url);
const readabilitySource = await readFile(require.resolve('@mozilla/readability/Readability.js'), 'utf8');
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function deadline(operation, ms, code) {
  let timer;
  try { return await Promise.race([operation, new Promise((_, reject) => { timer = setTimeout(() => reject(new ToolError(code, 504)), ms); })]); }
  finally { clearTimeout(timer); }
}

export class BrowserTools {
  constructor({ policy = new NetworkPolicy(), executablePath = '/usr/bin/chromium', maxSessions = 4, ttlMs = 600000, timeoutMs = 25000, engine = 'bing' } = {}) {
    Object.assign(this, { policy, executablePath, maxSessions, ttlMs, timeoutMs, engine });
    this.sandbox = true;
    this.sessions = new Map(); this.starting = null; this.browser = null; this.closed = false;
    this.timer = setInterval(() => this.reap().catch(() => {}), 15000); this.timer.unref();
  }
  async start() {
    if (this.browser?.isConnected()) return;
    if (this.starting) return this.starting;
    this.starting = this.launch().finally(() => { this.starting = null; });
    return this.starting;
  }
  async launch() {
    if (this.closed) throw new ToolError('service_stopping', 503);
    await this.stopBrowser();
    this.proxy = await startEgressProxy(this.policy);
    this.profile = await mkdtemp(join(tmpdir(), 'mneme-chromium-'));
    const args = ['--headless=new', '--disable-dev-shm-usage', '--disable-setuid-sandbox', '--remote-debugging-port=0', '--remote-debugging-address=127.0.0.1', `--user-data-dir=${this.profile}`, `--proxy-server=http://127.0.0.1:${this.proxy.port}`, '--proxy-bypass-list=<-loopback>', '--disable-quic', '--force-webrtc-ip-handling-policy=disable_non_proxied_udp', '--disable-background-networking', '--disable-component-update', '--disable-sync', '--no-first-run', '--no-default-browser-check', '--disable-extensions', '--disable-features=MediaRouter,DialMediaRouteProvider,OptimizationHints', '--password-store=basic', 'about:blank'];
    this.process = spawn(this.executablePath, args, { stdio: ['ignore', 'ignore', 'pipe'], env: { PATH: process.env.PATH, HOME: this.profile, LANG: 'C.UTF-8', TMPDIR: tmpdir() } });
    let failure = false;
    this.process.once('error', () => { failure = true; });
    this.process.stderr.resume();
    try {
      for (let n = 0; n < 150; n++) {
        if (failure || this.process.exitCode !== null || this.process.signalCode !== null) throw new ToolError('chromium_start_failed', 503);
        const endpoint = await readFile(join(this.profile, 'DevToolsActivePort'), 'utf8').catch(() => '');
        if (endpoint) {
          const [port, path] = endpoint.trim().split('\n');
          this.browser = await chromium.connectOverCDP(`ws://127.0.0.1:${port}${path}`, { timeout: 10000 });
          this.browser.on('disconnected', () => { this.browser = null; this.sessions.clear(); });
          // The startup tab consumes a renderer but is never used by a session.
          for (const context of this.browser.contexts()) for (const page of context.pages()) await page.close();
          return;
        }
        await delay(100);
      }
      throw new ToolError('chromium_start_timeout', 503);
    } catch (error) { await this.stopBrowser(); throw error; }
  }
  async session(id) {
    await deadline(this.start(), Math.min(10000, this.timeoutMs), 'browser_start_timeout');
    let session = this.sessions.get(id);
    if (session) return session;
    if (this.sessions.size >= this.maxSessions) throw new ToolError('browser_capacity', 429);
    // Reserve before awaiting context creation, so concurrent sessions cannot exceed the cap.
    session = { busy: true, lastUsed: Date.now(), context: null, page: null, refs: new Map(), refAttribute: `data-mneme-${randomUUID()}` };
    this.sessions.set(id, session);
    let expired = false;
    const initialize = async () => {
      const context = await this.browser.newContext({ viewport: { width: 1280, height: 900 }, locale: 'zh-CN', serviceWorkers: 'block', acceptDownloads: false, ignoreHTTPSErrors: false });
      if (expired || this.sessions.get(id) !== session) { await context.close(); throw new ToolError('browser_session_timeout', 504); }
      session.context = context;
      await context.route('**/*', async route => {
        try {
          const url = route.request().url();
          if (url.startsWith('data:') && !route.request().isNavigationRequest()) return route.continue();
          await this.policy.resolve(url);
          if (!['GET','HEAD'].includes(route.request().method())) {
            const target = new URL(url);
            const searchForm = (target.hostname === 'html.duckduckgo.com' && target.pathname === '/html/') || (target.hostname.endsWith('.bing.com') && target.pathname === '/search');
            const request = route.request(), headers = request.headers(), body = request.postDataBuffer();
            const jsonRequest = ['fetch', 'xhr'].includes(request.resourceType()) && /^application\/(?:[\w.-]+\+)?json(?:\s*;|$)/i.test(headers['content-type'] || '') && !headers.authorization && body?.length <= 65536;
            if (request.method() !== 'POST' || (!searchForm && !jsonRequest)) return route.abort('blockedbyclient');
            // Public SPAs often bootstrap through JSON POSTs. This does not
            // authorize form submissions or authenticated API requests.
            if (!searchForm) { try { JSON.parse(body.toString('utf8')); } catch { return route.abort('blockedbyclient'); } }
          }
          if (['media', 'font'].includes(route.request().resourceType())) return route.abort();
          await route.continue();
        } catch { await route.abort('blockedbyclient').catch(() => {}); }
      });
      await context.routeWebSocket('**/*', socket => socket.close());
      await context.clearPermissions();
      await context.addInitScript(() => { for (const name of ['RTCPeerConnection','webkitRTCPeerConnection','WebTransport']) Object.defineProperty(globalThis, name, { value: undefined, writable: false, configurable: false }); });
      context.on('page', page => { if (session.page && page !== session.page) page.close().catch(() => {}); });
      session.page = await context.newPage();
      if (expired) throw new ToolError('browser_session_timeout', 504);
      session.page.setDefaultTimeout(8000); session.page.setDefaultNavigationTimeout(this.timeoutMs - 2000);
      session.page.on('download', download => download.cancel().catch(() => {}));
      session.page.on('dialog', dialog => dialog.dismiss().catch(() => {}));
      session.busy = false;
      return session;
    };
    try { return await deadline(initialize(), Math.min(10000, this.timeoutMs), 'browser_session_timeout'); }
    catch (error) { expired = true; await this.closeSession(id); throw error; }
  }
  async closeSession(id) {
    const session = this.sessions.get(id); this.sessions.delete(id);
    if (session?.context) await session.context.close().catch(() => {});
  }
  async reap() {
    for (const [id, session] of this.sessions) if (!session.busy && Date.now() - session.lastUsed > this.ttlMs) await this.closeSession(id);
  }
  async execute(id, input) {
    const started = Date.now();
    if (input.action === 'close') { await this.closeSession(id); return { closed: true }; }
    if (!this.sessions.has(id) && !['search', 'navigate'].includes(input.action)) throw new ToolError('browser_session_missing_navigate_or_search', 409);
    const session = await this.session(id);
    if (session.busy) throw new ToolError('browser_session_busy', 409);
    session.busy = true; session.lastUsed = Date.now();
    let timer, timedOut = false, phase = 'action';
    const operation = async () => {
      const page = session.page;
      const element = () => { const handle = session.refs.get(input.element); if (!handle) throw new ToolError('element_not_found_read_again'); return handle; };
      let navigation = {};
      switch (input.action) {
        case 'search': {
          const engine = this.engine === 'duckduckgo' ? { url: 'https://html.duckduckgo.com/html/', selector: 'input[name="q"]' } : { url: 'https://www.bing.com/', selector: 'textarea[name="q"],input[name="q"]' };
          await this.policy.resolve(engine.url);
          await this.navigate(page, engine.url);
          const box = page.locator(engine.selector).first();
          await box.fill(input.query);
          // Register before Enter: waiting for the old document's load state can
          // otherwise succeed before the results navigation has even started.
          await Promise.all([page.waitForURL(url => url.pathname.includes('search') || url.searchParams.has('q'), { waitUntil: 'domcontentloaded', timeout: Math.min(10000, this.timeoutMs - 2000) }), box.press('Enter')]);
          await page.waitForSelector('li.b_algo, [data-testid="result"], article[data-testid]', { state: 'attached', timeout: 3500 }).catch(error => { if (error.name !== 'TimeoutError') throw error; });
          break;
        }
        case 'navigate': await this.policy.resolve(input.url); navigation = await this.navigate(page, input.url); break;
        case 'click': await element().evaluate(el => { if (el.tagName === 'A') el.removeAttribute('target'); }); await element().click(); await delay(500); break;
        case 'type': if (await element().evaluate(el => el.tagName === 'INPUT' && ['password','file'].includes(el.type))) throw new ToolError('sensitive_input_blocked'); await element().fill(input.text); if (input.press_enter) { await element().press('Enter'); await delay(500); } break;
        case 'scroll': await page.mouse.wheel(0, input.delta_y ?? 700); await delay(250); break;
        case 'back': await page.goBack({ waitUntil: 'domcontentloaded' }); break;
        case 'wait': await delay(input.wait_ms ?? 500); break;
        case 'read': case 'screenshot': break;
        default: throw new ToolError('unknown_action');
      }
      phase = 'snapshot';
      const snapshot = { ...await this.snapshot(session), ...navigation };
      if (input.action === 'search' && !snapshot.results.length && /unfortunately, bots|unusual traffic|异常流量|confirm.*human|人机验证/i.test(snapshot.text)) throw new ToolError('search_verification_required', 502);
      if (input.action === 'screenshot') {
        phase = 'screenshot';
        const bytes = await page.screenshot({ type: 'jpeg', quality: 75, fullPage: false, timeout: 8000 });
        if (bytes.length > 2 * 1024 * 1024) throw new ToolError('screenshot_too_large');
        snapshot.image = { mime: 'image/jpeg', data: bytes.toString('base64'), width: 1280, height: 900 };
      }
      return snapshot;
    };
    try {
      return await Promise.race([operation(), new Promise((_, reject) => { timer = setTimeout(() => { timedOut = true; reject(new ToolError('tool_timeout', 504)); }, Math.max(1, this.timeoutMs - (Date.now() - started))); })]);
    } catch (error) {
      // An outer deadline must cancel outstanding CDP work. Ordinary action
      // errors (including Playwright's own timeout) leave the page recoverable.
      if (timedOut || session.page.isClosed()) await this.closeSession(id);
      console.log(JSON.stringify({ event: 'browser_action_error', action: input.action, phase, session_closed: !this.sessions.has(id), error: error instanceof ToolError ? error.code : error.name === 'TimeoutError' ? 'browser_timeout' : 'browser_action_failed' }));
      if (error instanceof ToolError) throw error;
      throw new ToolError(error.name === 'TimeoutError' ? 'browser_timeout' : 'browser_action_failed', 502);
    } finally { clearTimeout(timer); session.busy = false; session.lastUsed = Date.now(); }
  }
  async navigate(page, url) {
    // Commit confirms this is the new document. A page can remain readable even
    // when a slow script or streaming response delays DOMContentLoaded forever.
    const response = await page.goto(url, { waitUntil: 'commit', timeout: Math.min(10000, this.timeoutMs - 2000) });
    let load_incomplete = false;
    try { await page.waitForLoadState('domcontentloaded', { timeout: Math.min(4000, Math.max(500, this.timeoutMs / 4)) }); }
    catch (error) { if (error.name !== 'TimeoutError') throw error; load_incomplete = true; }
    await page.waitForFunction(() => (document.body?.innerText.trim().length || 0) > 80 || document.querySelector('input:not([type=hidden]),textarea'), null, { timeout: Math.min(2500, this.timeoutMs / 4) }).catch(error => { if (error.name !== 'TimeoutError') throw error; });
    return { http_status: response?.status(), load_incomplete };
  }
  async snapshot(session) {
    const page = session.page;
    const url = page.url();
    // Validate the final URL too, including error documents after blocked redirects.
    if (url !== 'about:blank') await this.policy.resolve(url);
    const document = await page.evaluate(({ source, refAttribute }) => {
      // Mozilla Readability parses a clone; the original DOM stays available for interaction.
      const Readability = new Function(`${source}; return Readability;`)();
      const bodyText = document.body?.innerText || '';
      let article;
      if (document.getElementsByTagName('*').length < 12000) {
        try { article = new Readability(document.cloneNode(true), { charThreshold: 80, maxElemsToParse: 12000 }).parse(); } catch { /* Fall back to rendered text. */ }
      }
      const text = (article?.textContent || bodyText).replace(/\n{3,}/g, '\n\n').trim().slice(0, 14000);
      const links = [...document.querySelectorAll('a[href]')].map(a => ({ title: (a.innerText || a.getAttribute('aria-label') || '').trim().slice(0, 180), url: a.href })).filter(a => a.title && /^https?:/.test(a.url)).slice(0, 60);
      const results = [...document.querySelectorAll('li.b_algo, [data-testid="result"], article[data-testid]')].map(block => {
        const link = block.querySelector('h2 a, h3 a, a[data-testid="result-title-a"]');
        return link ? { title: (link.innerText || link.textContent || '').trim().slice(0, 180), url: (() => { try { const u = new URL(link.href); const encoded = u.searchParams.get('u'); if (u.hostname.endsWith('bing.com') && u.pathname.startsWith('/ck/') && encoded?.startsWith('a1')) { const decoded = atob(encoded.slice(2).replace(/-/g, '+').replace(/_/g, '/')); if (/^https?:\/\//.test(decoded)) return decoded; } } catch {} return link.href; })(), snippet: (block.innerText || block.textContent || '').trim().slice(0, 700) } : null;
      }).filter(Boolean).slice(0, 10);
      // One CDP evaluation replaces hundreds of sequential handle operations,
      // and locators do not keep detached DOM nodes alive between snapshots.
      for (const old of document.querySelectorAll(`[${refAttribute}]`)) old.removeAttribute(refAttribute);
      const elements = [];
      for (const el of [...document.querySelectorAll('a[href],button,input:not([type=hidden]),textarea,select,[role=button]')].slice(0, 300)) {
        if (elements.length >= 70) break;
        const bounds = el.getBoundingClientRect();
        if (bounds.width <= 0 || bounds.height <= 0 || getComputedStyle(el).visibility !== 'visible') continue;
        const ref = `e${elements.length + 1}`; el.setAttribute(refAttribute, ref);
        elements.push({ ref, tag: el.tagName.toLowerCase(), label: (el.getAttribute('aria-label') || el.innerText || el.getAttribute('placeholder') || el.getAttribute('name') || '').trim().slice(0, 120), type: el.getAttribute('type') || '', url: el.tagName === 'A' ? el.href : undefined });
      }
      return { title: (article?.title || document.title).slice(0, 300), text, links, results, elements, rendering_pending: text.length < 80 && elements.length === 0, rendering_limited: results.length > 0 && results.every(result => !bodyText.includes(result.title)) };
    }, { source: readabilitySource, refAttribute: session.refAttribute });
    session.refs.clear();
    for (const { ref } of document.elements) session.refs.set(ref, page.locator(`[${session.refAttribute}="${ref}"]`));
    return { url, ...document, untrusted: true, retrieved_at: new Date().toISOString() };
  }
  async reset() { for (const id of [...this.sessions.keys()]) await this.closeSession(id); }
  async stopBrowser() {
    await this.reset();
    const browser = this.browser; this.browser = null;
    // connectOverCDP().close() only disconnects the client. Ask the browser process
    // to exit before using signals, which container LSM policies can restrict.
    if (browser?.isConnected()) {
      await Promise.race([(async () => {
        const control = await browser.newBrowserCDPSession();
        await control.send('Browser.close');
      })().catch(() => {}), delay(3000)]);
    }
    await browser?.close().catch(() => {});
    const child = this.process;
    const alive = () => child && child.exitCode === null && child.signalCode === null;
    const waitExit = async () => {
      if (!alive()) return;
      let timer;
      await new Promise(resolve => {
        const done = () => { clearTimeout(timer); child.off('exit', done); resolve(); };
        child.once('exit', done); timer = setTimeout(done, 3000);
      });
    };
    await waitExit();
    for (const signal of ['SIGTERM', 'SIGKILL']) {
      if (!alive()) break;
      try { child.kill(signal); } catch { /* Still clean up sockets before reporting. */ }
      await waitExit();
    }
    await this.proxy?.close(); this.proxy = null;
    if (alive()) throw new ToolError('chromium_stop_failed', 503);
    this.process = null;
    if (this.profile) await rm(this.profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
  async close() { this.closed = true; clearInterval(this.timer); await this.stopBrowser(); }
}
