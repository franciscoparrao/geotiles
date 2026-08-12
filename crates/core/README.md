# geotiles-core

Library crate behind [`geotiles`](https://crates.io/crates/geotiles): web
map tile generation in Rust. Part of the SurtGIS ecosystem — it closes the
loop *analyze (SurtGIS) → publish to web (geotiles)*.

## What it does

- **Raster pyramids** → XYZ `z/x/y.png` trees, MBTiles 1.3, or **PMTiles**
  v3 (single-file archive served with HTTP Range requests), with
  nearest/bilinear sampling and area-averaged overviews.
- **Streaming I/O**: file-backed sources read only each tile's source
  window (memory scales with the tile, not the raster), and the PMTiles
  sink writes tiles straight to disk. Tile formats PNG / WebP lossless /
  JPEG lossy.
- **16 colour schemes** (terrain, grayscale, NDVI, Imhof relief, …) via
  [`surtgis-colormap`](https://crates.io/crates/surtgis-colormap), plus
  true-colour RGB(A) rendering with a per-band stretch.
- **Cloud Optimized GeoTIFF** writing (Float32 or byte RGB(A), deflate,
  2× overviews; passes GDAL's COG validator).
- **MVT vector tiles** from GeoJSON/GeoPackage/Shapefile/GeoParquet: clip,
  per-zoom Douglas-Peucker simplification, quantization, protobuf
  encoding, gzip.
- Parallel rendering (rayon) with a single writer thread per sink.

Inputs must be in EPSG:4326 or EPSG:3857.

## Example

```rust,no_run
use geotiles_core::{MbtilesSink, PyramidOptions, RasterSource, generate};

# fn main() -> geotiles_core::Result<()> {
let raster = surtgis_core::io::read_geotiff::<f64, _>("dem.tif", None)?;
let source = RasterSource::new(raster, None)?;
let mut sink = MbtilesSink::create("dem.mbtiles")?;
let stats = generate(&source, &PyramidOptions::default(), &mut sink, |_| {})?;
println!("{} tiles", stats.written);
# Ok(())
# }
```

Vector tiles follow the same shape via `VectorSource` + `generate_mvt`.

## License

MIT OR Apache-2.0
