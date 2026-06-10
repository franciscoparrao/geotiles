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
- [ ] COG writing
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
│   └── mbtiles.rs    MBTiles 1.3 sink (rusqlite, bundled)
└── cli/    geotiles binary (clap)
```

Raster I/O (`Raster<f64>`, GeoTIFF) comes from
[`surtgis-core`](../surtgis); colour mapping and PNG encoding from
`surtgis-colormap`.

## Known limitations (v0.1)

- Pixel-center point sampling: at very low zooms a small raster can fall
  between sample points, leaving some low-zoom tiles empty (e.g. z0/z2
  present no tile while z1 does). Viewers handle missing overlay tiles
  gracefully; area-averaged overviews are planned.
- Single-band sources only; RGB(A) GeoTIFF support is planned.
- No reprojection engine: only EPSG:4326 / EPSG:3857 inputs.

## Validation

Compared against `gdal2tiles`/tippecanoe conventions: tile addressing
(XYZ/TMS), MBTiles schema and metadata, and visual inspection in MapLibre.

## License

MIT OR Apache-2.0
