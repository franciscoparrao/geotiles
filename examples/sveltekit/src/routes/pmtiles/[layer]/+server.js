// Serve PMTiles archives by HTTP Range, proxying the file bytes.
//
// PMTiles is a single-file archive: MapLibre (via the `pmtiles` JS
// protocol) reads the header, directory and the exact tile bytes it needs
// using range requests. A static host with Range support serves the file
// as-is; this endpoint reproduces that so the same .pmtiles files work
// from any SvelteKit deployment (serverless included).
//
// Usage: GET /pmtiles/<layer>  with a `Range: bytes=a-b` header →
// 206 Partial Content (or 200 for the full file). MapLibre points the
// `pmtiles://` protocol at `${origin}/pmtiles/<layer>`.

import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { readFile, stat } from 'node:fs/promises';

const DATA_DIR = join(dirname(fileURLToPath(import.meta.url)), '../../../../data');
const SAFE = /^[a-z0-9_-]+$/i;

// layer -> { path, size } cached to avoid stat() per request.
const cache = new Map();

async function info(layer) {
  if (cache.has(layer)) return cache.get(layer);
  if (!SAFE.test(layer)) throw new Error(`invalid layer name: ${layer}`);

  const path = join(DATA_DIR, `${layer}.pmtiles`);
  const st = await stat(path).catch(() => null);
  if (!st) throw new Error(`no pmtiles ${layer} (run generate-tiles.sh --pmtiles)`);

  const entry = { path, size: st.size };
  cache.set(layer, entry);
  return entry;
}

/**
 * Serve the PMTiles file for `layer`, honoring a single `Range: bytes=…`
 * header. Returns a 206 with `Content-Range` when a range is requested,
 * otherwise the whole file.
 */
export async function GET({ params, request }) {
  let f;
  try {
    f = await info(params.layer);
  } catch (e) {
    return new Response(e.message, { status: 404 });
  }

  const range = request.headers.get('range');
  const common = {
    'content-type': 'application/octet-stream',
    'accept-ranges': 'bytes',
    'cache-control': 'public, max-age=3600'
  };

  if (range?.startsWith('bytes=')) {
    const [startRaw, endRaw] = range.slice(6).split(',')[0].split('-');
    const start = Number(startRaw) || 0;
    const end = endRaw ? Math.min(Number(endRaw), f.size - 1) : f.size - 1;
    if (start >= f.size || start > end) {
      return new Response(null, { status: 416, headers: { 'content-range': `bytes */${f.size}` } });
    }
    const length = end - start + 1;
    const fd = await readFile(f.path);
    return new Response(fd.subarray(start, end + 1), {
      status: 206,
      headers: {
        ...common,
        'content-length': String(length),
        'content-range': `bytes ${start}-${end}/${f.size}`
      }
    });
  }

  const fd = await readFile(f.path);
  return new Response(fd, { status: 200, headers: { ...common, 'content-length': String(f.size) } });
}
