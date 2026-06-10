//! Error types for geotiles-core.

use std::path::PathBuf;

/// Errors produced while generating or packaging tile pyramids.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying raster I/O or processing error from surtgis-core.
    #[error(transparent)]
    Core(#[from] surtgis_core::Error),

    /// PNG encoding failure.
    #[error("tile encoding failed: {0}")]
    Encode(String),

    /// SQLite error while writing MBTiles.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    /// Filesystem error writing an XYZ tile tree.
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The input raster cannot be tiled (missing/unsupported CRS, empty, …).
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

/// Convenience alias used across the crate.
pub type Result<T> = std::result::Result<T, Error>;
