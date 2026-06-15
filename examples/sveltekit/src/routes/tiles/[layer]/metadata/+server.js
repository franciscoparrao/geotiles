import { error, json } from '@sveltejs/kit';
import { metadata } from '$lib/server/mbtiles.js';

/**
 * GET /tiles/<layer>/metadata — tileset metadata as JSON.
 *
 * Normalizes the MBTiles `metadata` table into the few fields a client
 * needs to self-configure: format, bounds, zoom range, and (for vector
 * tilesets) the parsed `vector_layers`.
 */
export function GET({ params }) {
  let meta;
  try {
    meta = metadata(params.layer);
  } catch (e) {
    throw error(404, e.message);
  }

  const bounds = meta.bounds?.split(',').map(Number) ?? [-180, -85, 180, 85];
  let vectorLayers;
  try {
    vectorLayers = meta.json ? JSON.parse(meta.json).vector_layers : undefined;
  } catch {
    vectorLayers = undefined;
  }

  return json({
    name: meta.name,
    format: meta.format ?? 'png',
    minzoom: Number(meta.minzoom ?? 0),
    maxzoom: Number(meta.maxzoom ?? 14),
    bounds,
    vector_layers: vectorLayers
  });
}
