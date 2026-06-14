//! XYZ directory sink: writes tiles as `{root}/{z}/{x}/{y}.png`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::mercator::TileCoord;
use crate::pyramid::{PyramidMetadata, TileSink};

/// Writes a slippy-map directory tree plus a `metadata.json` summary.
#[derive(Debug)]
pub struct XyzSink {
    root: PathBuf,
    ext: &'static str,
}

impl XyzSink {
    /// Create a PNG tile sink, ensuring `root` exists.
    pub fn create(root: impl AsRef<Path>) -> Result<Self> {
        Self::with_extension(root, "png")
    }

    /// Create a sink writing tiles with the given file extension
    /// (e.g. `"pbf"` for vector tiles).
    pub fn with_extension(root: impl AsRef<Path>, ext: &'static str) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(|source| Error::Io { path: root.clone(), source })?;
        Ok(Self { root, ext })
    }

    /// Root directory of the tile tree.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl TileSink for XyzSink {
    fn put(&mut self, coord: TileCoord, png: &[u8]) -> Result<()> {
        let dir = self.root.join(coord.z.to_string()).join(coord.x.to_string());
        fs::create_dir_all(&dir).map_err(|source| Error::Io { path: dir.clone(), source })?;
        let path = dir.join(format!("{}.{}", coord.y, self.ext));
        fs::write(&path, png).map_err(|source| Error::Io { path, source })
    }

    fn finalize(&mut self, meta: &PyramidMetadata) -> Result<()> {
        let (w, s, e, n) = meta.bounds_lonlat;
        // For vector tilesets, embed the MBTiles-style `json` field so a
        // viewer can discover the source-layer name; raster omits it.
        let json_field = match &meta.json {
            Some(j) => format!(",\n  \"json\": {j:?}"),
            None => String::new(),
        };
        // Minimal TileJSON-style summary so viewers can self-configure.
        let json = format!(
            concat!(
                "{{\n",
                "  \"name\": {name:?},\n",
                "  \"format\": \"{format}\",\n",
                "  \"minzoom\": {minz},\n",
                "  \"maxzoom\": {maxz},\n",
                "  \"bounds\": [{w}, {s}, {e}, {n}],\n",
                "  \"scheme\": \"xyz\"{json_field}\n",
                "}}\n"
            ),
            name = meta.name,
            format = meta.format,
            minz = meta.min_zoom,
            maxz = meta.max_zoom,
            w = w,
            s = s,
            e = e,
            n = n,
            json_field = json_field,
        );
        let path = self.root.join("metadata.json");
        fs::write(&path, json).map_err(|source| Error::Io { path, source })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_tree_and_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = XyzSink::create(dir.path().join("tiles")).unwrap();
        sink.put(TileCoord { z: 3, x: 2, y: 5 }, b"fake-png").unwrap();
        sink.finalize(&PyramidMetadata {
            name: "test".into(),
            bounds_lonlat: (-71.0, -34.0, -70.0, -33.0),
            min_zoom: 0,
            max_zoom: 3,
            format: "png",
            json: None,
        })
        .unwrap();

        let tile = dir.path().join("tiles/3/2/5.png");
        assert_eq!(fs::read(tile).unwrap(), b"fake-png");
        let meta = fs::read_to_string(dir.path().join("tiles/metadata.json")).unwrap();
        assert!(meta.contains("\"maxzoom\": 3"));
        assert!(meta.contains("\"scheme\": \"xyz\""));
    }
}
