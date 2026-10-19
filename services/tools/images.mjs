import http from 'node:http';
import https from 'node:https';
import sharp from 'sharp';
import { NetworkPolicy, ToolError } from './security.mjs';

export async function normalizeImage(bytes, sticker = false) {
  if (!Buffer.isBuffer(bytes) || !bytes.length) throw new ToolError('invalid_image');
  try {
    const image = sharp(bytes, { limitInputPixels: sticker ? 40_000_000 : 20_000_000, animated: sticker, failOn: 'warning' });
    const metadata = await image.metadata();
    if (!['png', 'jpeg', 'webp', 'gif'].includes(metadata.format)) throw new ToolError('unsupported_image');
    if ((metadata.pages || 1) > 100 || metadata.width * metadata.height > 40_000_000) throw new ToolError('image_too_large');
    const pipeline = image.rotate().resize({ width: sticker ? 512 : 2048, height: sticker ? 512 : 2048, fit: 'inside', withoutEnlargement: true });
    const format = sticker ? metadata.format : 'jpeg';
    if (!sticker) pipeline.flatten({ background: '#ffffff' });
    const { data, info } = await pipeline.toFormat(format, format === 'jpeg' ? { quality: 82 } : {}).toBuffer({ resolveWithObject: true });
    if (data.length > 2 * 1024 * 1024) throw new ToolError('image_too_large');
    return { mime: `image/${format}`, data: data.toString('base64'), width: info.width, height: info.pageHeight || info.height };
  } catch (error) { if (error instanceof ToolError) throw error; throw new ToolError('invalid_image'); }
}

export async function stickerPreview(bytes) {
  try {
    const metadata = await sharp(bytes, { animated: true, limitInputPixels: 40_000_000, failOn: 'warning' }).metadata();
    if (!['png', 'jpeg', 'gif', 'webp'].includes(metadata.format) || (metadata.pages || 1) > 100) throw new ToolError('invalid_image');
    const pages = metadata.pages || 1;
    const frames = [...new Set([0, Math.floor((pages - 1) / 2), pages - 1])];
    const tiles = await Promise.all(frames.map(async (page, i) => ({
      input: await sharp(bytes, { page, pages: 1, limitInputPixels: 40_000_000, failOn: 'warning' })
        .rotate().resize(256, 256, { fit: 'contain', background: '#fff' }).flatten({ background: '#fff' }).jpeg({ quality: 85 }).toBuffer(),
      left: i * 256, top: 0,
    })));
    const data = await sharp({ create: { width: 256 * frames.length, height: 256, channels: 3, background: '#fff' } }).composite(tiles).jpeg({ quality: 85 }).toBuffer();
    return { mime: 'image/jpeg', data: data.toString('base64'), width: 256 * frames.length, height: 256 };
  } catch (error) { if (error instanceof ToolError) throw error; throw new ToolError('invalid_image'); }
}

// Resolve and pin every hop, preserving TLS hostname verification. No cookies or tokens
// are sent to attachment hosts, and redirect targets must pass the same public-IP policy.
export async function fetchImage(input, policy = new NetworkPolicy()) {
  const signal = AbortSignal.timeout(15000);
  let url = input;
  for (let hop = 0; hop < 4; hop++) {
    const target = await policy.resolve(url);
    const result = await new Promise((resolve, reject) => {
      const transport = target.url.protocol === 'https:' ? https : http;
      const request = transport.get(target.url, { signal, agent: false, lookup: (_, options, callback) => callback(null, options.all ? [{ address: target.address, family: target.family }] : target.address, target.family) }, response => {
        if ([301, 302, 303, 307, 308].includes(response.statusCode) && response.headers.location) {
          response.destroy(); resolve({ redirect: new URL(response.headers.location, target.url).href }); return;
        }
        if (response.statusCode !== 200 || Number(response.headers['content-length'] || 0) > 8 * 1024 * 1024) {
          response.destroy(); reject(new ToolError('image_download_failed')); return;
        }
        const chunks = []; let length = 0;
        response.on('data', chunk => { length += chunk.length; if (length > 8 * 1024 * 1024) { response.destroy(new ToolError('image_too_large')); return; } chunks.push(chunk); });
        response.on('end', () => resolve({ bytes: Buffer.concat(chunks) }));
        response.on('error', reject);
      });
      request.on('error', reject);
    });
    if (result.bytes) return normalizeImage(result.bytes);
    url = result.redirect;
  }
  throw new ToolError('image_download_failed');
}
