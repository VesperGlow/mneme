import { test } from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import sharp from 'sharp';
import { fetchImage, normalizeImage } from '../images.mjs';
import { NetworkPolicy } from '../security.mjs';

test('custom stickers preserve GIF frames and transparency; vision accepts GIF first frame', async () => {
  const raw = Buffer.from([255, 0, 0, 255, 0, 255, 0, 255]);
  const gif = await sharp(raw, { raw: { width: 1, height: 2, channels: 4, pageHeight: 1 } }).gif({ delay: [100, 200], loop: 0 }).toBuffer();
  const sticker = await normalizeImage(gif, true);
  assert.equal(sticker.mime, 'image/gif');
  assert.equal(sticker.height, 1);
  const saved = await sharp(Buffer.from(sticker.data, 'base64'), { animated: true }).metadata();
  assert.equal(saved.pages, 2);
  assert.deepEqual(saved.delay, [100, 200]);
  const vision = await normalizeImage(gif);
  assert.equal(vision.mime, 'image/jpeg');
  const png = await sharp({ create: { width: 800, height: 600, channels: 4, background: '#ffffff00' } }).png().toBuffer();
  const resized = await normalizeImage(png, true);
  assert.equal(resized.width, 512);
  assert.equal(resized.height, 384);
  assert.equal((await sharp(Buffer.from(resized.data, 'base64')).metadata()).hasAlpha, true);
  for (const bytes of [Buffer.from('<svg xmlns="http://www.w3.org/2000/svg"/>'), Buffer.from('GIF89a'), Buffer.alloc(0)]) await assert.rejects(normalizeImage(bytes, true));
});

test('attachment downloads validate each redirect, cap bytes and strip request credentials', async () => {
  const png = await sharp({ create: { width: 2, height: 2, channels: 3, background: '#fff' } }).png().toBuffer();
  let reached = 0;
  const server = http.createServer((req, res) => {
    assert.equal(req.headers.authorization, undefined); assert.equal(req.headers.cookie, undefined);
    if (req.url === '/redirect') { res.writeHead(302, { location: 'http://127.0.0.1/private' }); res.end(); }
    else if (req.url === '/large') { res.writeHead(200, { 'content-length': 9 * 1024 * 1024 }); res.end(); }
    else { reached++; res.end(png); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  class FixturePolicy extends NetworkPolicy {
    async resolve(input) {
      const url = new URL(input);
      if (url.hostname === 'images.example') return { url: new URL(`http://images.example:${server.address().port}${url.pathname}`), address: '127.0.0.1', family: 4 };
      return super.resolve(input);
    }
  }
  try {
    assert.equal((await fetchImage('http://images.example/ok', new FixturePolicy())).mime, 'image/jpeg');
    await assert.rejects(fetchImage('http://images.example/redirect', new FixturePolicy()));
    await assert.rejects(fetchImage('http://images.example/large', new FixturePolicy()));
    await assert.rejects(fetchImage(`http://127.0.0.1:${server.address().port}/private`));
    assert.equal(reached, 1);
  } finally { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
});
