//! # geotiles-core
//!
//! Web map tile generation: XYZ pyramids and MBTiles packaging from
//! geospatial rasters. Part of the SurtGIS ecosystem — raster I/O and
//! colour mapping are reused from `surtgis-core` / `surtgis-colormap`.
//!
//! ## Pipeline
//!
//! 1. Wrap a [`Raster<f64>`](surtgis_core::Raster) in a [`RasterSource`]
//!    (CRS detection: EPSG:4326 or EPSG:3857).
//! 2. Pick [`PyramidOptions`] (zoom range, colour scheme, resampling).
//! 3. Call [`generate`] with a [`TileSink`]: [`XyzSink`] for a `z/x/y.png`
//!    tree, [`MbtilesSink`] for a single `.mbtiles` file.
//!
//! ```no_run
//! use geotiles_core::{MbtilesSink, PyramidOptions, RasterSource, generate};
//!
//! # fn main() -> geotiles_core::Result<()> {
//! let raster = surtgis_core::io::read_geotiff::<f64, _>("dem.tif", None)?;
//! let source = RasterSource::new(raster, None)?;
//! let mut sink = MbtilesSink::create("dem.mbtiles")?;
//! let stats = generate(&source, &PyramidOptions::default(), &mut sink, |_| {})?;
//! println!("{} tiles", stats.written);
//! # Ok(())
//! # }
//! ```

pub mod cog;
pub mod error;
pub mod mbtiles;
pub mod mercator;
pub mod pyramid;
pub mod source;
pub mod xyz;

pub use cog::{CogCompression, CogInfo, CogOptions, write_cog};
pub use error::{Error, Result};
pub use mbtiles::MbtilesSink;
pub use mercator::{TileCoord, TileRange};
pub use pyramid::{
    PyramidMetadata, PyramidOptions, PyramidStats, TileSink, count_tiles, generate,
};
pub use source::{RasterSource, Resampling, SourceCrs};
pub use xyz::XyzSink;

// Re-export the colour schemes so CLI/users don't need surtgis-colormap directly.
pub use surtgis_colormap::ColorScheme;
