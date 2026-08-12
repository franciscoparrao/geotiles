# geotiles + SvelteKit example

Serve [geotiles](https://crates.io/crates/geotiles) output from a SvelteKit
app. Two backends are supported:

- **PMTiles** (default): one single-file archive per tileset, read by the
  browser with HTTP Range requests through the `pmtiles://` protocol. The
  app just proxies the file bytes (`/pmtiles/<layer>`); no per-tile
  endpoint needed. This is the pattern for static/serverless hosting.
- **MBTiles**: an endpoint reads tiles straight from an MBTiles file (one
  file, not thousands of loose tiles).

Both render a raster relief layer and a vector (MVT) overlay. Switch with
`?backend=pmtiles` (default) or `?backend=mbtiles`.

The reusable pieces are [`src/lib/server/mbtiles.js`](src/lib/server/mbtiles.js)
(the tile endpoint) and [`src/routes/pmtiles/[layer]/+server.js`](src/routes/pmtiles/[layer]/+server.js)
(the Range proxy).

## Run it

```bash
# 1. Generate the demo tilesets (needs the geotiles binary on PATH)
cargo install geotiles            # if you don't have it
./generate-tiles.sh               # writes relief/hidrografia in .mbtiles + .pmtiles

# 2. Start the app
npm install
npm run dev
```

Open the printed URL. The map fits to the relief bounds with the vector
hidrografía layer (polygons / lines / points) on top.

`generate-tiles.sh` needs `data/dem.tif` (a small EPSG:4326 GeoTIFF). A DEM
in another CRS must be reprojected first:
`gdalwarp -t_srs EPSG:4326 your_dem.tif data/dem.tif`.

## How it works

### PMTiles backend

```
data/<layer>.pmtiles  ──►  src/routes/pmtiles/[layer]/+server.js   (Range proxy)
                               │  HTTP Range: bytes=a-b → 206 Partial Content
                               ▼
  +page.svelte  ──►  pmtiles.Protocol → MapLibre (pmtiles:// URLs)
```

Key detail: the endpoint **proxies the file bytes** honoring the `Range`
header (`Content-Range` / `Accept-Ranges` / 206), so the `pmtiles` JS client
reads the header, directory and only the tile bytes it needs. Any static
host with Range support (S3, R2, GitHub Pages) works identically — the
endpoint exists to make the same files work from any SvelteKit deployment.

### MBTiles backend

```
data/<layer>.mbtiles  ──►  src/lib/server/mbtiles.js   (better-sqlite3, cached)
                              │  flip XYZ y → TMS row
                              ▼
  GET /tiles/<layer>/<z>/<x>/<y>      → tile bytes (png / pbf+gzip)
  GET /tiles/<layer>/metadata         → format, bounds, zooms, vector_layers
                              │
                              ▼
  +page.svelte  ──►  MapLibre raster + vector sources
```

Details handled by the MBTiles endpoint:

- **TMS row flip**: MBTiles stores rows bottom-up; the XYZ `y` is flipped.
- **Content types**: `image/png` for raster, `application/x-protobuf` for
  MVT, with `content-encoding: gzip` (geotiles gzips vector tiles).
- **Empty tiles**: returns `204` so MapLibre shows blank, not an error.
- **Caching**: one read-only SQLite connection per tileset, kept open.

## Files not in git

`data/*.mbtiles`, `data/*.pmtiles` and `data/dem.tif` are generated locally
(see `.gitignore`); only `data/hidrografia.geojson` is versioned.
