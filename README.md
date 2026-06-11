# geotiles

Web map tile generation in Rust: XYZ pyramids and MBTiles from geospatial
rasters. A "tippecanoe lite" for the SurtGIS ecosystem — closes the loop
*analyze (SurtGIS) → publish to web (geotiles)*.

## Status

v0.1 (raster pipeline). Vector tiles (MVT) and COG writing are planned.

- [x] Raster → XYZ tile tree (`z/x/y.png`) with resampling and full pyramid
- [x] MBTiles 1.3 packaging (SQLite, TMS row order)
- [x] 16 colour schemes (terrain, grayscale, NDVI, Imhof relief, …) via `surtgis-colormap`
- [x] Parallel rendering (rayon) with a single writer thread
- [x] Area-averaged sampling at overview zooms (no empty low-zoom tiles)
- [x] COG writing (Float32, deflate, 2× overviews; passes GDAL's COG validator)
- [ ] Vector tiles (MVT) from GeoJSON/GPKG

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
│   └── cog.rs        Cloud Optimized GeoTIFF writer (own TIFF encoder)
└── cli/    geotiles binary (clap)
```

Raster I/O (`Raster<f64>`, GeoTIFF) comes from
[`surtgis-core`](https://crates.io/crates/surtgis-core); colour mapping and
PNG encoding from
[`surtgis-colormap`](https://crates.io/crates/surtgis-colormap). Both are
pulled from crates.io, so the repo builds standalone.

## Known limitations (v0.1)

- Single-band sources only; RGB(A) GeoTIFF support is planned.
- No reprojection engine: only EPSG:4326 / EPSG:3857 inputs.
- COG output is always Float32 single band (matches the analysis-raster
  use case; byte/RGB COGs are planned together with RGB(A) tiling).

## Validation

Compared against `gdal2tiles`/tippecanoe conventions: tile addressing
(XYZ/TMS), MBTiles schema and metadata, and visual inspection in MapLibre.

## License

MIT OR Apache-2.0
