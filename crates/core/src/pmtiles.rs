//! PMTiles v3 sink.
//!
//! A single-file tile archive served with HTTP range requests from static
//! hosting (no server needed). The canonical [`pmtiles`] crate (stadiamaps)
//! handles header, directory, dedup and clustering.
//!
//! The crate writes header/metadata at [`PmTilesWriter::create`] time, so a
//! streaming sink needs the tile format *and* the pyramid metadata (bounds,
//! zooms, vector layers) before any tile is written. [`PmtilesSink::create`]
//! takes the format; [`TileSink`]-style callers supply the rest through
//! [`PmtilesSink::prepare`] before the first [`put`](TileSink::put), and
//! [`finalize`](TileSink::finalize) then only closes the writer — tiles are
//! streamed to disk as they are produced, never buffered.
//!
//! Tile coordinates are passed through as XYZ — the PMTiles spec assigns
//! tile IDs from z/x/y directly (no TMS flip), geotiles' native orientation.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use pmtiles::{Compression, PmTilesStreamWriter, PmTilesWriter, TileCoord as PmTileCoord, TileType};

use crate::error::{Error, Result};
use crate::mercator::TileCoord;
use crate::pyramid::{PyramidMetadata, TileSink};

/// PMTiles writer that streams tiles to disk as they are produced.
///
/// The `pmtiles` crate boxes its compressors as `dyn Compressor`, which the
/// trait does not declare `Send` (even though every concrete compressor is).
/// The pipeline hands the sink to a single writer thread and never touches
/// it concurrently, so forwarding `Send` is sound: the inner writer is only
/// ever accessed from that one thread.
pub struct PmtilesSink {
    path: PathBuf,
    format: String,
    writer: Option<PmTilesStreamWriter<BufWriter<File>>>,
}

// SAFETY: the writer is accessed only from the single pipeline writer thread
// (see `run_tiles`); no concurrent access ever occurs. Every concrete
// compressor stored inside is `Send`.
unsafe impl Send for PmtilesSink {}

impl PmtilesSink {
    /// Create (or overwrite) a PMTiles file for the given tile format.
    ///
    /// `format` is a geotiles format string ("png"/"webp"/"jpeg"/"pbf");
    /// it fixes the header's tile type and compression.
    pub fn create(path: impl AsRef<Path>, format: &str) -> Result<Self> {
        let path = path.as_ref();
        if path.exists() {
            std::fs::remove_file(path).map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            format: format.to_string(),
            writer: None,
        })
    }

    /// Give the writer the pyramid metadata before the first tile.
    ///
    /// The PMTiles header/metadata must be written before tile data, so the
    /// caller must call this before [`TileSink::put`]. Calling it after the
    /// writer is open (or twice) is an error.
    pub fn prepare(&mut self, meta: &PyramidMetadata) -> Result<()> {
        if self.writer.is_some() {
            return Err(Error::InvalidInput(
                "pmtiles: prepare() called after writer opened".into(),
            ));
        }
        if meta.format != self.format {
            return Err(Error::InvalidInput(format!(
                "pmtiles: format mismatch (sink {}, metadata {})",
                self.format, meta.format
            )));
        }

        let (w, s, e, n) = meta.bounds_lonlat;
        let json = match &meta.json {
            Some(layers) => layers.clone(),
            None => format!(
                r#"{{"name":"{}","type":"overlay","version":"1.0.0"}}"#,
                meta.name
            ),
        };

        let tile_type = tile_type_for(meta.format);
        let mut builder = PmTilesWriter::new(tile_type)
            .min_zoom(meta.min_zoom)
            .max_zoom(meta.max_zoom)
            .bounds(w, s, e, n)
            .center((w + e) / 2.0, (s + n) / 2.0)
            .center_zoom(meta.min_zoom)
            .metadata(&json);

        // MVT tiles are already gzip-compressed by the encoder, so declare
        // gzip and write raw. Raster codecs are already final bytes.
        if matches!(tile_type, TileType::Mvt) {
            builder = builder.tile_compression(Compression::Gzip);
        }

        let file = File::create(&self.path).map_err(|source| Error::Io {
            path: self.path.clone(),
            source,
        })?;
        let writer = builder
            .create(BufWriter::new(file))
            .map_err(pmtiles_err)?;
        self.writer = Some(writer);
        Ok(())
    }

    fn write_tile(&mut self, coord: TileCoord, data: &[u8]) -> Result<()> {
        let writer = self.writer.as_mut().ok_or_else(|| {
            Error::InvalidInput(
                "pmtiles: put() called before prepare() (no writer)".into(),
            )
        })?;
        let tcoord = PmTileCoord::new(coord.z, coord.x, coord.y).map_err(pmtiles_err)?;
        if matches!(tile_type_for(&self.format), TileType::Mvt) {
            writer.add_raw_tile(tcoord, data).map_err(pmtiles_err)
        } else {
            writer.add_tile(tcoord, data).map_err(pmtiles_err)
        }
    }
}

/// Map a geotiles format string ("png"/"webp"/"jpeg"/"pbf") to a PMTiles
/// tile type.
fn tile_type_for(format: &str) -> TileType {
    match format {
        "png" => TileType::Png,
        "webp" => TileType::Webp,
        "jpeg" => TileType::Jpeg,
        "pbf" | "mvt" => TileType::Mvt,
        _ => TileType::Unknown,
    }
}

fn pmtiles_err(e: impl std::fmt::Display) -> Error {
    Error::Encode(format!("pmtiles: {e}"))
}

impl TileSink for PmtilesSink {
    fn put(&mut self, coord: TileCoord, png: &[u8]) -> Result<()> {
        self.write_tile(coord, png)
    }

    fn finalize(&mut self, meta: &PyramidMetadata) -> Result<()> {
        // If the caller never called prepare() (e.g. generated zero tiles),
        // still produce a valid archive with the metadata from finalize.
        if self.writer.is_none() {
            self.prepare(meta)?;
        }
        let writer = self.writer.take().expect("writer present after prepare");
        writer.finalize().map_err(pmtiles_err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> PyramidMetadata {
        PyramidMetadata {
            name: "dem".into(),
            bounds_lonlat: (-72.0, -34.0, -70.0, -32.0),
            min_zoom: 0,
            max_zoom: 1,
            format: "png",
            json: None,
        }
    }

    #[test]
    fn writes_valid_pmtiles() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.pmtiles");
        let mut sink = PmtilesSink::create(&path, "png").unwrap();
        sink.prepare(&meta()).unwrap();
        sink.put(TileCoord { z: 0, x: 0, y: 0 }, b"png-bytes-0").unwrap();
        sink.put(TileCoord { z: 1, x: 0, y: 0 }, b"png-bytes-1").unwrap();
        sink.finalize(&meta()).unwrap();
        drop(sink);

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..7], b"PMTiles", "magic number");
        assert_eq!(bytes[7], 3, "version 3");
    }

    #[test]
    fn finalize_without_prepare_still_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.pmtiles");
        let mut sink = PmtilesSink::create(&path, "png").unwrap();
        sink.finalize(&meta()).unwrap();
        drop(sink);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..7], b"PMTiles", "magic number");
    }

    #[test]
    fn writes_mvt_tiles_gzip_declared() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.pmtiles");
        let mut sink = PmtilesSink::create(&path, "pbf").unwrap();
        sink.prepare(&PyramidMetadata {
            name: "vec".into(),
            bounds_lonlat: (-72.0, -34.0, -70.0, -32.0),
            min_zoom: 0,
            max_zoom: 0,
            format: "pbf",
            json: Some(r#"{"vector_layers":[]}"#.into()),
        })
        .unwrap();
        sink.put(TileCoord { z: 0, x: 0, y: 0 }, b"gzipped-pbf").unwrap();
        sink.finalize(&PyramidMetadata {
            name: "vec".into(),
            bounds_lonlat: (-72.0, -34.0, -70.0, -32.0),
            min_zoom: 0,
            max_zoom: 0,
            format: "pbf",
            json: Some(r#"{"vector_layers":[]}"#.into()),
        })
        .unwrap();
        drop(sink);

        let bytes = std::fs::read(&path).unwrap();
        // Header layout (spec §3.2): magic(7) + version(1) + 11×8-byte
        // offsets/counts (root, meta, leaf, data, addressed, entries,
        // contents) = 96 bytes, then clustered(1), internal compression(1),
        // tile compression(1), tile type(1).
        assert_eq!(bytes[96], 1, "must be clustered");
        assert_eq!(bytes[97], 2, "internal compression must be gzip (0x02)");
        assert_eq!(bytes[98], 2, "tile compression must be gzip (0x02)");
        assert_eq!(bytes[99], 1, "tile type must be MVT (0x01)");
    }

    #[test]
    fn format_mismatch_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.pmtiles");
        let mut sink = PmtilesSink::create(&path, "png").unwrap();
        let mut m = meta();
        m.format = "pbf";
        assert!(sink.prepare(&m).is_err());
    }
}
