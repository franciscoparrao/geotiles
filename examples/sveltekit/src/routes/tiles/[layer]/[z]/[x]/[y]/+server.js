import { error } from '@sveltejs/kit';
import { tile } from '$lib/server/mbtiles.js';

const CONTENT_TYPE = {
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  webp: 'image/webp',
  pbf: 'application/x-protobuf'
};

/** GET /tiles/<layer>/<z>/<x>/<y> — one XYZ tile from data/<layer>.mbtiles. */
export function GET({ params }) {
  const z = Number(params.z);
  const x = Number(params.x);
  // The y segment may carry an extension (e.g. "5.png"); strip it.
  const y = Number(String(params.y).split('.')[0]);
  if (![z, x, y].every(Number.isInteger)) throw error(400, 'bad tile coords');

  let result;
  try {
    result = tile(params.layer, z, x, y);
  } catch (e) {
    throw error(404, e.message);
  }
  // 204: MapLibre treats an empty body as a blank tile (no error spam).
  if (!result) return new Response(null, { status: 204 });

  const headers = {
    'content-type': CONTENT_TYPE[result.format] ?? 'application/octet-stream',
    'cache-control': 'public, max-age=3600'
  };
  // geotiles stores MVT tiles gzip-compressed; advertise it so the browser
  // (and MapLibre) transparently inflate them.
  if (result.format === 'pbf') headers['content-encoding'] = 'gzip';

  return new Response(result.data, { headers });
}
