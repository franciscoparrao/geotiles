# geotiles

Web map tile generation in Rust: XYZ pyramids and MBTiles from geospatial
rasters. A "tippecanoe lite" for the SurtGIS ecosystem — closes the loop
*analyze (SurtGIS) → publish to web (geotiles)*.

## Status

v0.2 — raster (XYZ/MBTiles/COG, RGB) and vector tiles (MVT).

- [x] Raster → XYZ tile tree (`z/x/y.png`) with resampling and full pyramid
- [x] MBTiles 1.3 packaging (SQLite, TMS row order)
- [x] 16 colour schemes (terrain, grayscale, NDVI, Imhof relief, …) via `surtgis-colormap`
- [x] Parallel rendering (rayon) with a single writer thread
- [x] Area-averaged sampling at overview zooms (no empty low-zoom tiles)
- [x] COG writing (Float32, deflate, 2× overviews; passes GDAL's COG validator)
- [x] RGB(A) sources: multiband GeoTIFF reader, true-colour tiles
  (`--bands 1,2,3[,4]`) and byte RGB(A) COG output
- [x] Vector tiles (MVT) from GeoJSON/GPKG — clip, per-zoom simplification,
  MBTiles (`pbf`+gzip) or XYZ `.pbf` tree; matches GDAL's MVT output
- [x] Lossless WebP raster tiles (`--format webp`) — ~20 % smaller than PNG,
  pixel-identical, pure Rust (no libwebp)

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

# Lossless WebP tiles instead of PNG (~20% smaller, pixel-identical)
geotiles raster dem.tif -o dem.mbtiles --scheme terrain --format webp

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
│   ├── source.rs     RasterSource: CRS detection, nearest/bilinear sampling
│   ├── pyramid.rs    tile rendering + rayon orchestration, TileSink trait
│   ├── xyz.rs        z/x/y.png directory sink
│   ├── mbtiles.rs    MBTiles 1.3 sink (rusqlite, bundled)
│   ├── cog.rs        Cloud Optimized GeoTIFF writer (own TIFF encoder)
│   ├── io.rs         multiband GeoTIFF reader (tiff crate + geo tags)
│   ├── vector.rs     VectorSource: GeoJSON/GPKG load, reprojection, layers
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
- RGB stretch is one global `--range` for all bands (per-band ranges and
  non-linear stretches are out of scope).
- Readers decode the full dataset into memory (same approach as
  surtgis-core's native readers); streaming reads are future work.
- **Vector tiles**: no tippecanoe-style feature dropping for planet-scale
  data — geotiles targets thematic layers (thousands to hundreds of
  thousands of features). One GeoPackage table per layer spec (use several
  `path#table` specs to pull multiple tables from one .gpkg).

## Validation

Compared against `gdal2tiles`/tippecanoe conventions: tile addressing
(XYZ/TMS), MBTiles schema and metadata, and visual inspection in MapLibre.

## License

MIT OR Apache-2.0
