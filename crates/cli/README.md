# geotiles

Web map tile generation in Rust: XYZ pyramids and MBTiles from geospatial
rasters. A "tippecanoe lite" for the SurtGIS ecosystem — closes the loop
*analyze (SurtGIS) → publish to web (geotiles)*.

## Status

v0.5 — raster (XYZ/MBTiles/PMTiles/COG, RGB, WebP/JPEG) and vector tiles
(MVT) with streaming I/O and feature dropping.

- [x] Raster → XYZ tile tree (`z/x/y.png`) with resampling and full pyramid
- [x] MBTiles 1.3 packaging (SQLite, TMS row order)
- [x] **PMTiles v3** output (single file, HTTP Range requests, no server)
- [x] 16 colour schemes (terrain, grayscale, NDVI, Imhof relief, …) via `surtgis-colormap`
- [x] Tile formats: PNG, **WebP lossless**, **JPEG lossy**
- [x] Parallel rendering (rayon) with a single writer thread
- [x] Area-averaged sampling at overview zooms (no empty low-zoom tiles)
- [x] **Streaming raster input** (memory ∝ tile window, not the raster)
- [x] **Streaming PMTiles sink** (tiles written to disk as produced)
- [x] COG writing (Float32, deflate, 2× overviews; passes GDAL's COG validator)
- [x] RGB(A) sources: multiband GeoTIFF reader, true-colour tiles
  (`--bands 1,2,3[,4]`, per-band stretch with `--band-range`) and byte
  RGB(A) COG output
- [x] Vector tiles (MVT) from GeoJSON/GPKG/Shapefile/GeoParquet/FlatGeobuf —
  clip, per-zoom simplification, MBTiles (`pbf`+gzip) or XYZ `.pbf` tree;
  matches GDAL's MVT output
- [x] **Feature dropping** (`--drop-densest-as-needed`): keeps dense tiles
  under a byte budget (tippecanoe-style)
- [x] **FlatGeobuf R-tree bbox filter** (`#bbox=minx,miny,maxx,maxy`): read
  only intersecting features from large files

## Install

```bash
cargo install geotiles
```

## Usage

```bash
# Inspect a raster: bounds, CRS, suggested native zoom
geotiles info dem.tif

# GeoTIFF → MBTiles (zooms 0..native, terrain colours)
geotiles raster dem.tif -o dem.mbtiles --scheme terrain

# GeoTIFF → XYZ directory, fixed zooms and stretch
geotiles raster dem.tif -o tiles/ --min-zoom 8 --max-zoom 14 --range 0,2500

# Available colour schemes
geotiles raster --list-schemes x -o x

# Rewrite as Cloud Optimized GeoTIFF (Float32 + internal overviews)
geotiles cog dem.tif -o dem_cog.tif

# RGB imagery: true-colour tiles and byte RGB COG
geotiles raster ortho.tif -o ortho.mbtiles --bands 1,2,3
geotiles cog ortho.tif -o ortho_cog.tif --bands 1,2,3

# Vector tiles (MVT) from GeoJSON or GeoPackage
geotiles vector cuencas.geojson -o cuencas.mbtiles --max-zoom 14

# Multi-layer tileset: each input is a layer ([name=]path[#gpkg_table])
geotiles vector cuencas.geojson red=hidro.gpkg#rios estaciones.geojson \
  -o hidrografia.mbtiles --name hidrografia

# Vector/Shapefile/GeoParquet/FlatGeobuf inputs
geotiles vector cuencas.shp rios=rios.geojson -o hidrografia.mbtiles
geotiles vector hidrografia=hidrografia.parquet -o hidrografia.mbtiles
geotiles vector hidrografia=hidrografia.fgb -o hidrografia.mbtiles

# FlatGeobuf with an R-tree bbox filter (file CRS): only intersecting features
geotiles vector "pts=big.fgb#bbox=-71.0,-33.0,-69.5,-31.5" -o east.mbtiles

# Drop the densest features in tiles that exceed 100 KB (tippecanoe-style)
geotiles vector dense.geojson -o dense.mbtiles \
  --drop-densest-as-needed --max-tile-size 102400

# PMTiles: single file for static hosting (serve with any Range-capable host)
geotiles raster dem.tif -o dem.pmtiles --scheme terrain
geotiles vector cuencas.geojson -o cuencas.pmtiles --max-zoom 14

# JPEG tiles for imagery (no alpha; transparency → black)
geotiles raster ortho.tif -o ortho.mbtiles --bands 1,2,3 --format jpeg

# Per-band stretch for RGB (Landsat-style composites)
geotiles raster scene.tif -o rgb.mbtiles --bands 4,3,2 \
  --band-range 0,4000;0,4000;0,4000
```

Inputs must be in EPSG:4326 or EPSG:3857 (`--source-crs` overrides
detection). Reproject anything else first, e.g.
`gdalwarp -t_srs EPSG:4326 in.tif out.tif`.

Nodata pixels become transparent; fully empty tiles are skipped.

Try the output in a browser: `examples/viewer.html` (MapLibre) reads an XYZ
tree served with any static file server.

## Architecture

```
crates/
├── core/   geotiles-core: mercator math, sampling, pyramid, sinks
│   ├── mercator.rs   XYZ tile math (EPSG:3857), TMS flip
│   ├── source.rs     RasterSource: CRS detection, nearest/bilinear sampling, file-backed windows
│   ├── pyramid.rs    tile rendering + rayon orchestration, TileSink trait, window budget
│   ├── xyz.rs        z/x/y.png directory sink
│   ├── mbtiles.rs    MBTiles 1.3 sink (rusqlite, bundled)
│   ├── pmtiles.rs    PMTiles v3 sink (streams to disk, range-ready)
│   ├── cog.rs        Cloud Optimized GeoTIFF writer (own TIFF encoder)
│   ├── io.rs         GeoTIFF reader + windowed chunk reads + streaming min/max
│   ├── vector.rs     VectorSource: GeoJSON/GPKG/Shapefile/GeoParquet, reprojection, layers
│   └── mvt.rs        MVT pyramid: clip, simplify, quantize, encode, gzip
└── cli/    geotiles binary (clap)
```

Raster I/O (`Raster<f64>`, GeoTIFF) comes from
[`surtgis-core`](https://crates.io/crates/surtgis-core); colour mapping and
PNG encoding from
[`surtgis-colormap`](https://crates.io/crates/surtgis-colormap). Both are
pulled from crates.io, so the repo builds standalone.

## Known limitations

- No reprojection engine: only EPSG:4326 / EPSG:3857 inputs.
- **Vector tiles**: no tippecanoe-style feature dropping for planet-scale
  data — geotiles targets thematic layers (thousands to hundreds of
  thousands of features). One geometry type per Shapefile (cuencas/rios/
  estaciones as separate layers).

## Validation

Compared against `gdal2tiles`/tippecanoe conventions: tile addressing
(XYZ/TMS), MBTiles schema and metadata, and visual inspection in MapLibre.

## License

MIT OR Apache-2.0
