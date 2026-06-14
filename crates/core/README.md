# geotiles-core

Library crate behind [`geotiles`](https://crates.io/crates/geotiles): web
map tile generation in Rust. Part of the SurtGIS ecosystem — it closes the
loop *analyze (SurtGIS) → publish to web (geotiles)*.

## What it does

- **Raster pyramids** → XYZ `z/x/y.png` trees or MBTiles 1.3, with
  nearest/bilinear sampling and area-averaged overviews.
- **16 colour schemes** (terrain, grayscale, NDVI, Imhof relief, …) via
  [`surtgis-colormap`](https://crates.io/crates/surtgis-colormap), plus
  true-colour RGB(A) rendering from multiband sources.
- **Cloud Optimized GeoTIFF** writing (Float32 or byte RGB(A), deflate,
  2× overviews; passes GDAL's COG validator).
- **MVT vector tiles** from GeoJSON/GeoPackage: clip, per-zoom
  Douglas-Peucker simplification, quantization, protobuf encoding, gzip.
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
