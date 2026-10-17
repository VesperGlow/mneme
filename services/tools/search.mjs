import { ToolError } from './security.mjs';

const endpoint = 'https://api.tavily.com/search';
const maxResponseBytes = 1024 * 1024;
const bounded = (value, limit) => typeof value === 'string' ? value.slice(0, limit) : '';

export class TavilySearch {
  #keys;
  #nextKey = 0;

  constructor({ apiKeys = process.env.MNEME_TAVILY_API_KEY || '', fetchImpl = fetch, timeoutMs = 25000 } = {}) {
    this.#keys = apiKeys.split('/').map(key => key.trim()).filter(Boolean);
    this.fetch = fetchImpl;
    this.timeoutMs = timeoutMs;
  }

  get configured() { return this.#keys.length > 0; }

  async execute(query) {
    if (typeof query !== 'string' || !query.trim() || query.length > 500) throw new ToolError('invalid_search_query');
    if (!this.configured) throw new ToolError('tavily_api_key_required', 503);
    // Reserve the next key before awaiting I/O, including concurrent and failed calls.
    const key = this.#keys[this.#nextKey];
    this.#nextKey = (this.#nextKey + 1) % this.#keys.length;
    const signal = AbortSignal.timeout(this.timeoutMs);
    try {
      const response = await this.fetch(endpoint, {
        method: 'POST', redirect: 'error', signal,
        headers: { Authorization: `Bearer ${key}`, 'Content-Type': 'application/json' },
        body: JSON.stringify({ query: query.trim(), search_depth: 'basic', max_results: 10, include_answer: false, include_raw_content: false, include_images: false }),
      });
      if (!response.ok) {
        await response.body?.cancel();
        const code = [401, 403].includes(response.status) ? 'tavily_auth_failed'
          : response.status === 429 ? 'tavily_rate_limited'
          : [432, 433].includes(response.status) ? 'tavily_quota_exceeded' : 'tavily_search_failed';
        throw new ToolError(code, 502);
      }
      const chunks = [];
      let size = 0;
      for await (const chunk of response.body) {
        size += chunk.length;
        if (size > maxResponseBytes) throw new ToolError('tavily_response_too_large', 502);
        chunks.push(chunk);
      }
      const data = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      if (!data || !Array.isArray(data.results)) throw new ToolError('tavily_invalid_response', 502);
      const results = data.results.filter(item => {
        if (!item || typeof item.url !== 'string' || item.url.length > 4096) return false;
        try { const url = new URL(item.url); return ['http:', 'https:'].includes(url.protocol) && !url.username && !url.password; } catch { return false; }
      }).slice(0, 10).map(item => ({ title: bounded(item.title, 180), url: item.url, snippet: bounded(item.content, 1200) }));
      return {
        provider: 'tavily', query: query.trim(), results,
        links: results.map(({ title, url }) => ({ title, url })),
        text: results.map(item => `${item.title}\n${item.url}\n${item.snippet}`).join('\n\n').slice(0, 14000),
        untrusted: true, retrieved_at: new Date().toISOString(),
      };
    } catch (error) {
      if (error instanceof ToolError) throw error;
      if (signal.aborted) throw new ToolError('tavily_timeout', 504);
      if (error instanceof SyntaxError) throw new ToolError('tavily_invalid_response', 502);
      // Upstream bodies and transport errors may contain credentials; expose only codes.
      throw new ToolError('tavily_search_failed', 502);
    }
  }
}
