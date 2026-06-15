// Serve tiles from geotiles MBTiles files.
//
// One .mbtiles per named tileset under data/<layer>.mbtiles. Connections
// and metadata are cached per layer. This is the reusable piece: drop it
// into any SvelteKit app to publish geotiles output from a single file
// instead of a directory of thousands of PNG/PBF tiles.

import Database from 'better-sqlite3';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { existsSync } from 'node:fs';

const DATA_DIR = join(dirname(fileURLToPath(import.meta.url)), '../../../data');

// layer -> { db, meta, stmt }
const cache = new Map();

// MBTiles layer names are filenames; reject anything that could escape DATA_DIR.
const SAFE = /^[a-z0-9_-]+$/i;

function open(layer) {
  if (cache.has(layer)) return cache.get(layer);
  if (!SAFE.test(layer)) throw new Error(`invalid layer name: ${layer}`);

  const path = join(DATA_DIR, `${layer}.mbtiles`);
  if (!existsSync(path)) throw new Error(`no tileset ${layer} (run generate-tiles.sh)`);

  const db = new Database(path, { readonly: true, fileMustExist: true });
  const meta = Object.fromEntries(
    db.prepare('SELECT name, value FROM metadata').all().map((r) => [r.name, r.value])
  );
  const stmt = db.prepare(
    'SELECT tile_data AS d FROM tiles WHERE zoom_level=? AND tile_column=? AND tile_row=?'
  );
  const entry = { db, meta, stmt };
  cache.set(layer, entry);
  return entry;
}

/** Tileset metadata as a plain object (format, bounds, minzoom, …). */
export function metadata(layer) {
  return open(layer).meta;
}

/**
 * Fetch one XYZ tile. Returns `{ data: Buffer, format: string }`, or
 * `null` when the tile is absent (empty area).
 *
 * MBTiles stores rows in TMS order, so the XYZ `y` is flipped here.
 */
export function tile(layer, z, x, y) {
  const { meta, stmt } = open(layer);
  const tmsY = (1 << z) - 1 - y;
  const row = stmt.get(z, x, tmsY);
  if (!row) return null;
  return { data: row.d, format: meta.format ?? 'png' };
}
