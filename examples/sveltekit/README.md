# geotiles + SvelteKit example

Serve [geotiles](https://crates.io/crates/geotiles) output from a SvelteKit
app: an endpoint reads tiles straight from an **MBTiles** file (one file,
not thousands of loose tiles) and MapLibre renders them. Shows both a raster
relief layer and a vector (MVT) overlay from the same app.

This is the pattern to copy into your own SvelteKit sites — the reusable
piece is [`src/lib/server/mbtiles.js`](src/lib/server/mbtiles.js) plus the
tile endpoint.

## Run it

```bash
# 1. Generate the demo tilesets (needs the geotiles binary on PATH)
cargo install geotiles            # if you don't have it
./generate-tiles.sh               # writes data/relief.mbtiles + data/hidrografia.mbtiles

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

Key details handled by the endpoint:

- **TMS row flip**: MBTiles stores rows bottom-up; the XYZ `y` is flipped.
- **Content types**: `image/png` for raster, `application/x-protobuf` for
  MVT, with `content-encoding: gzip` (geotiles gzips vector tiles).
- **Empty tiles**: returns `204` so MapLibre shows blank, not an error.
- **Caching**: one read-only SQLite connection per tileset, kept open.

## Files not in git

`data/*.mbtiles` and `data/dem.tif` are generated locally
(see `.gitignore`); only `data/hidrografia.geojson` is versioned.
