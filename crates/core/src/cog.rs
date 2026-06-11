//! Cloud Optimized GeoTIFF (COG) writer.
//!
//! Produces a classic little-endian TIFF with the COG layout:
//! all IFDs at the start of the file, tile data afterwards with overview
//! levels before the full-resolution level, so HTTP range readers can
//! fetch the header plus the smallest overview cheaply.
//!
//! Scope (v0.1): single band, Float32 samples, deflate or no compression,
//! EPSG:4326 / EPSG:3857 georeferencing, 2× overviews by valid-cell
//! averaging. This complements the COG *reader* in SurtGIS.

use std::io::Write as _;
use std::path::Path;

use flate2::Compression as Flate;
use flate2::write::ZlibEncoder;
use surtgis_core::Raster;

use crate::error::{Error, Result};
use crate::source::SourceCrs;

/// Tile data compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CogCompression {
    /// Zlib/Adobe deflate (TIFF compression 8).
    #[default]
    Deflate,
    /// No compression.
    None,
}

/// Options for [`write_cog`].
#[derive(Debug, Clone)]
pub struct CogOptions {
    /// Internal tile edge in pixels; must be a multiple of 16 (default 512).
    pub tile_size: u32,
    /// Tile compression (default deflate).
    pub compression: CogCompression,
    /// CRS to record. `None`: taken from the raster (4326/3857 only).
    pub crs: Option<SourceCrs>,
}

impl Default for CogOptions {
    fn default() -> Self {
        Self { tile_size: 512, compression: CogCompression::Deflate, crs: None }
    }
}

/// Summary returned by [`write_cog`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CogInfo {
    /// Number of resolution levels (1 full + overviews).
    pub levels: usize,
    /// Total tiles across all levels.
    pub tiles: u64,
    /// Bytes written.
    pub file_size: u64,
}

// ── TIFF constants ──────────────────────────────────────────────────────

const T_NEW_SUBFILE_TYPE: u16 = 254;
const T_IMAGE_WIDTH: u16 = 256;
const T_IMAGE_LENGTH: u16 = 257;
const T_BITS_PER_SAMPLE: u16 = 258;
const T_COMPRESSION: u16 = 259;
const T_PHOTOMETRIC: u16 = 262;
const T_SAMPLES_PER_PIXEL: u16 = 277;
const T_TILE_WIDTH: u16 = 322;
const T_TILE_LENGTH: u16 = 323;
const T_TILE_OFFSETS: u16 = 324;
const T_TILE_BYTE_COUNTS: u16 = 325;
const T_SAMPLE_FORMAT: u16 = 339;
const T_MODEL_PIXEL_SCALE: u16 = 33550;
const T_MODEL_TIEPOINT: u16 = 33922;
const T_GEO_KEY_DIRECTORY: u16 = 34735;
const T_GDAL_NODATA: u16 = 42113;

const TYPE_SHORT: u16 = 3;
const TYPE_LONG: u16 = 4;
const TYPE_ASCII: u16 = 2;
const TYPE_DOUBLE: u16 = 12;

/// One IFD entry; values longer than 4 bytes live in an out-of-line blob.
struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    /// Inline value (≤ 4 bytes, already padded) or out-of-line payload.
    value: Vec<u8>,
}

impl Entry {
    fn short(tag: u16, v: u16) -> Self {
        let mut value = v.to_le_bytes().to_vec();
        value.resize(4, 0);
        Self { tag, typ: TYPE_SHORT, count: 1, value }
    }
    fn long(tag: u16, v: u32) -> Self {
        Self { tag, typ: TYPE_LONG, count: 1, value: v.to_le_bytes().to_vec() }
    }
    fn longs(tag: u16, vs: &[u32]) -> Self {
        let value = vs.iter().flat_map(|v| v.to_le_bytes()).collect();
        Self { tag, typ: TYPE_LONG, count: vs.len() as u32, value }
    }
    fn shorts(tag: u16, vs: &[u16]) -> Self {
        let mut value: Vec<u8> = vs.iter().flat_map(|v| v.to_le_bytes()).collect();
        if value.len() < 4 {
            value.resize(4, 0);
        }
        Self { tag, typ: TYPE_SHORT, count: vs.len() as u32, value }
    }
    fn doubles(tag: u16, vs: &[f64]) -> Self {
        let value = vs.iter().flat_map(|v| v.to_le_bytes()).collect();
        Self { tag, typ: TYPE_DOUBLE, count: vs.len() as u32, value }
    }
    fn ascii(tag: u16, s: &str) -> Self {
        let mut value = s.as_bytes().to_vec();
        value.push(0);
        let count = value.len() as u32;
        if value.len() < 4 {
            value.resize(4, 0);
        }
        Self { tag, typ: TYPE_ASCII, count, value }
    }

    fn payload_len(&self) -> usize {
        if self.value.len() > 4 { self.value.len() } else { 0 }
    }
}

/// One resolution level with its encoded tiles.
struct Level {
    width: u32,
    height: u32,
    tiles: Vec<Vec<u8>>,
}

/// Halve a raster by averaging each 2×2 block's valid cells.
///
/// NaN and the nodata value count as invalid; a block with no valid cell
/// becomes NaN (encoded as the nodata of the level pyramid).
fn downsample_half(raster: &Raster<f64>) -> Raster<f64> {
    let rows = raster.rows().div_ceil(2);
    let cols = raster.cols().div_ceil(2);
    let mut out = Raster::filled(rows, cols, f64::NAN);
    for r in 0..rows {
        for c in 0..cols {
            let mut acc = 0.0;
            let mut n = 0u32;
            for (dr, dc) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (sr, sc) = (r * 2 + dr, c * 2 + dc);
                if sr < raster.rows()
                    && sc < raster.cols()
                    && let Ok(v) = raster.get(sr, sc)
                    && !v.is_nan()
                    && !raster.is_nodata(v)
                {
                    acc += v;
                    n += 1;
                }
            }
            if n > 0 {
                let _ = out.set(r, c, acc / n as f64);
            }
        }
    }
    out
}

/// Cut one level into padded `tile_size²` Float32 tiles and compress them.
fn encode_level_tiles(
    raster: &Raster<f64>,
    fill: f32,
    tile_size: u32,
    compression: CogCompression,
) -> Result<Vec<Vec<u8>>> {
    let ts = tile_size as usize;
    let tiles_x = raster.cols().div_ceil(ts);
    let tiles_y = raster.rows().div_ceil(ts);

    let coords: Vec<(usize, usize)> = (0..tiles_y)
        .flat_map(|ty| (0..tiles_x).map(move |tx| (ty, tx)))
        .collect();

    let encode_one = |&(ty, tx): &(usize, usize)| -> Result<Vec<u8>> {
        let mut raw = Vec::with_capacity(ts * ts * 4);
        for i in 0..ts {
            let row = ty * ts + i;
            for j in 0..ts {
                let col = tx * ts + j;
                let v = if row < raster.rows() && col < raster.cols() {
                    let v = raster.get(row, col).unwrap_or(f64::NAN);
                    if v.is_nan() || raster.is_nodata(v) { fill } else { v as f32 }
                } else {
                    fill
                };
                raw.extend_from_slice(&v.to_le_bytes());
            }
        }
        match compression {
            CogCompression::None => Ok(raw),
            CogCompression::Deflate => {
                let mut enc = ZlibEncoder::new(Vec::new(), Flate::default());
                enc.write_all(&raw)
                    .and_then(|_| enc.finish())
                    .map_err(|e| Error::Encode(format!("deflate: {e}")))
            }
        }
    };

    #[cfg(feature = "parallel")]
    {
        use rayon::prelude::*;
        coords.par_iter().map(encode_one).collect()
    }
    #[cfg(not(feature = "parallel"))]
    coords.iter().map(encode_one).collect()
}

fn geo_key_directory(crs: SourceCrs) -> Vec<u16> {
    // Header: version 1.1.0, N keys. Keys sorted by id, each
    // (id, location, count, value); location 0 = inline SHORT value.
    match crs {
        // ModelType 2 = geographic, RasterType 1 = PixelIsArea, GCS 4326.
        SourceCrs::LonLat => vec![
            1, 1, 0, 3,
            1024, 0, 1, 2,
            1025, 0, 1, 1,
            2048, 0, 1, 4326,
        ],
        // ModelType 1 = projected, PCS 3857.
        SourceCrs::Mercator => vec![
            1, 1, 0, 3,
            1024, 0, 1, 1,
            1025, 0, 1, 1,
            3072, 0, 1, 3857,
        ],
    }
}

/// Write `raster` as a Cloud Optimized GeoTIFF.
///
/// The CRS must be EPSG:4326 or EPSG:3857 (detected from the raster or set
/// via [`CogOptions::crs`]). Sample type is Float32; the raster's nodata
/// (or NaN) is recorded in the `GDAL_NODATA` tag.
pub fn write_cog(raster: &Raster<f64>, path: impl AsRef<Path>, opts: &CogOptions) -> Result<CogInfo> {
    if raster.rows() == 0 || raster.cols() == 0 {
        return Err(Error::InvalidInput("empty raster".into()));
    }
    if opts.tile_size == 0 || !opts.tile_size.is_multiple_of(16) {
        return Err(Error::InvalidInput(format!(
            "tile size must be a positive multiple of 16, got {}",
            opts.tile_size
        )));
    }
    let crs = match opts.crs {
        Some(crs) => crs,
        None => match raster.crs().and_then(|c| c.epsg()) {
            Some(4326) => SourceCrs::LonLat,
            Some(3857) | Some(900_913) => SourceCrs::Mercator,
            Some(other) => {
                return Err(Error::InvalidInput(format!(
                    "unsupported CRS EPSG:{other} for COG output; reproject first"
                )));
            }
            None => {
                return Err(Error::InvalidInput(
                    "raster has no CRS; pass CogOptions::crs explicitly".into(),
                ));
            }
        },
    };

    let nodata = raster.nodata().unwrap_or(f64::NAN);
    let fill: f32 = if nodata.is_nan() { f32::NAN } else { nodata as f32 };

    // ── Build the level pyramid (full res + 2× overviews) ──────────────
    let mut rasters: Vec<Raster<f64>> = vec![];
    let mut current = None::<Raster<f64>>;
    loop {
        let base = current.as_ref().unwrap_or(raster);
        let (h, w) = (base.rows() as u32, base.cols() as u32);
        if current.is_some() {
            rasters.push(current.clone().unwrap());
        }
        if w <= opts.tile_size && h <= opts.tile_size {
            break;
        }
        current = Some(downsample_half(base));
    }

    let mut levels = Vec::with_capacity(1 + rasters.len());
    levels.push(Level {
        width: raster.cols() as u32,
        height: raster.rows() as u32,
        tiles: encode_level_tiles(raster, fill, opts.tile_size, opts.compression)?,
    });
    for r in &rasters {
        levels.push(Level {
            width: r.cols() as u32,
            height: r.rows() as u32,
            tiles: encode_level_tiles(r, fill, opts.tile_size, opts.compression)?,
        });
    }

    // ── Build IFDs (offsets resolved after sizing) ──────────────────────
    let compression_tag: u16 = match opts.compression {
        CogCompression::Deflate => 8,
        CogCompression::None => 1,
    };
    let gt = raster.transform();

    let mut ifds: Vec<Vec<Entry>> = Vec::with_capacity(levels.len());
    for (li, level) in levels.iter().enumerate() {
        let mut e = vec![
            Entry::long(T_NEW_SUBFILE_TYPE, if li == 0 { 0 } else { 1 }),
            Entry::long(T_IMAGE_WIDTH, level.width),
            Entry::long(T_IMAGE_LENGTH, level.height),
            Entry::short(T_BITS_PER_SAMPLE, 32),
            Entry::short(T_COMPRESSION, compression_tag),
            Entry::short(T_PHOTOMETRIC, 1),
            Entry::short(T_SAMPLES_PER_PIXEL, 1),
            Entry::long(T_TILE_WIDTH, opts.tile_size),
            Entry::long(T_TILE_LENGTH, opts.tile_size),
            // Placeholders; patched once the data layout is known.
            Entry::longs(T_TILE_OFFSETS, &vec![0u32; level.tiles.len()]),
            Entry::longs(
                T_TILE_BYTE_COUNTS,
                &level.tiles.iter().map(|t| t.len() as u32).collect::<Vec<_>>(),
            ),
            Entry::short(T_SAMPLE_FORMAT, 3),
        ];
        if li == 0 {
            // Pixel scale uses |pixel_height|; row direction is encoded by
            // the tiepoint at the top-left corner.
            e.push(Entry::doubles(
                T_MODEL_PIXEL_SCALE,
                &[gt.to_gdal()[1], gt.to_gdal()[5].abs(), 0.0],
            ));
            e.push(Entry::doubles(
                T_MODEL_TIEPOINT,
                &[0.0, 0.0, 0.0, gt.to_gdal()[0], gt.to_gdal()[3], 0.0],
            ));
            e.push(Entry::shorts(T_GEO_KEY_DIRECTORY, &geo_key_directory(crs)));
            e.push(Entry::ascii(T_GDAL_NODATA, &format!("{nodata}")));
        }
        e.sort_by_key(|en| en.tag);
        ifds.push(e);
    }

    // ── Lay out the file: header | IFDs+payloads | tile data ───────────
    let mut offset = 8u32; // after TIFF header
    let mut ifd_offsets = Vec::with_capacity(ifds.len());
    let mut payload_offsets = Vec::with_capacity(ifds.len());
    for entries in &ifds {
        ifd_offsets.push(offset);
        let dir_size = 2 + entries.len() as u32 * 12 + 4;
        let payload_base = offset + dir_size;
        let payload_len: u32 = entries.iter().map(|e| e.payload_len() as u32).sum();
        payload_offsets.push(payload_base);
        offset = payload_base + payload_len;
    }

    // Tile data: overview levels first (smallest last in `levels` order is
    // levels[N-1]); write smallest → largest, full resolution last.
    let mut tile_offsets: Vec<Vec<u32>> = vec![vec![]; levels.len()];
    for li in (0..levels.len()).rev() {
        for tile in &levels[li].tiles {
            tile_offsets[li].push(offset);
            offset += tile.len() as u32;
        }
    }

    // Patch the tile-offset entries.
    for (li, entries) in ifds.iter_mut().enumerate() {
        for e in entries.iter_mut() {
            if e.tag == T_TILE_OFFSETS {
                e.value = tile_offsets[li].iter().flat_map(|v| v.to_le_bytes()).collect();
            }
        }
    }

    // ── Serialize ───────────────────────────────────────────────────────
    let mut buf: Vec<u8> = Vec::with_capacity(offset as usize);
    buf.extend_from_slice(b"II");
    buf.extend_from_slice(&42u16.to_le_bytes());
    buf.extend_from_slice(&ifd_offsets[0].to_le_bytes());

    for (li, entries) in ifds.iter().enumerate() {
        debug_assert_eq!(buf.len() as u32, ifd_offsets[li]);
        buf.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        let mut payload_cursor = payload_offsets[li];
        let mut payloads: Vec<&[u8]> = vec![];
        for e in entries {
            buf.extend_from_slice(&e.tag.to_le_bytes());
            buf.extend_from_slice(&e.typ.to_le_bytes());
            buf.extend_from_slice(&e.count.to_le_bytes());
            if e.value.len() > 4 {
                buf.extend_from_slice(&payload_cursor.to_le_bytes());
                payload_cursor += e.value.len() as u32;
                payloads.push(&e.value);
            } else {
                debug_assert_eq!(e.value.len(), 4);
                buf.extend_from_slice(&e.value);
            }
        }
        // Next-IFD pointer.
        let next = if li + 1 < ifds.len() { ifd_offsets[li + 1] } else { 0 };
        buf.extend_from_slice(&next.to_le_bytes());
        for p in payloads {
            buf.extend_from_slice(p);
        }
    }

    let mut tiles_total = 0u64;
    for li in (0..levels.len()).rev() {
        for (ti, tile) in levels[li].tiles.iter().enumerate() {
            debug_assert_eq!(buf.len() as u32, tile_offsets[li][ti]);
            buf.extend_from_slice(tile);
            tiles_total += 1;
        }
    }

    std::fs::write(path.as_ref(), &buf).map_err(|source| Error::Io {
        path: path.as_ref().to_path_buf(),
        source,
    })?;

    Ok(CogInfo { levels: levels.len(), tiles: tiles_total, file_size: buf.len() as u64 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use surtgis_core::{CRS, GeoTransform};

    fn gradient(rows: usize, cols: usize) -> Raster<f64> {
        let mut r = Raster::new(rows, cols);
        for i in 0..rows {
            for j in 0..cols {
                r.set(i, j, (i * cols + j) as f64).unwrap();
            }
        }
        r.set_transform(GeoTransform::new(-71.0, -32.0, 0.001, -0.001));
        r.set_crs(Some(CRS::from_epsg(4326)));
        r
    }

    #[test]
    fn downsample_averages_blocks() {
        let r = gradient(4, 4);
        let d = downsample_half(&r);
        assert_eq!(d.shape(), (2, 2));
        // Block (0,0): values 0,1,4,5 → 2.5.
        assert!((d.get(0, 0).unwrap() - 2.5).abs() < 1e-9);
    }

    #[test]
    fn downsample_skips_nodata() {
        let mut r = gradient(2, 2);
        r.set_nodata(Some(-1.0));
        r.set(0, 0, -1.0).unwrap();
        let d = downsample_half(&r);
        // Valid: 1, 2, 3 → 2.0.
        assert!((d.get(0, 0).unwrap() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn writes_readable_cog_with_overviews() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.tif");
        // 600×700 with 256-px tiles → needs at least one overview level.
        let r = gradient(600, 700);
        let opts = CogOptions { tile_size: 256, ..Default::default() };
        let info = write_cog(&r, &path, &opts).unwrap();
        assert!(info.levels >= 2, "expected overviews, got {} level(s)", info.levels);

        // Read back with surtgis-core's GeoTIFF reader (full-res IFD).
        let back = surtgis_core::io::read_geotiff::<f64, _>(&path, None).unwrap();
        assert_eq!(back.shape(), (600, 700));
        assert!((back.get(0, 0).unwrap() - 0.0).abs() < 1e-6);
        assert!((back.get(599, 699).unwrap() - (600.0 * 700.0 - 1.0)).abs() < 1e-6);
    }

    #[test]
    fn rejects_bad_tile_size() {
        let dir = tempfile::tempdir().unwrap();
        let r = gradient(8, 8);
        let opts = CogOptions { tile_size: 100, ..Default::default() };
        assert!(write_cog(&r, dir.path().join("x.tif"), &opts).is_err());
    }

    #[test]
    fn ifds_precede_tile_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.tif");
        let r = gradient(600, 700);
        let opts = CogOptions { tile_size: 256, ..Default::default() };
        write_cog(&r, &path, &opts).unwrap();

        // Walk the IFD chain; every IFD offset must sit before the first
        // tile-data offset (the COG layout requirement).
        let bytes = std::fs::read(&path).unwrap();
        let u16at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
        let u32at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
        let mut ifd = u32at(4) as usize;
        let mut max_ifd_end = 0usize;
        let mut min_tile_offset = usize::MAX;
        while ifd != 0 {
            let n = u16at(ifd) as usize;
            max_ifd_end = max_ifd_end.max(ifd + 2 + n * 12 + 4);
            for k in 0..n {
                let e = ifd + 2 + k * 12;
                if u16at(e) == T_TILE_OFFSETS {
                    let count = u32at(e + 4) as usize;
                    let first = if count == 1 {
                        u32at(e + 8) as usize
                    } else {
                        u32at(u32at(e + 8) as usize) as usize
                    };
                    min_tile_offset = min_tile_offset.min(first);
                }
            }
            ifd = u32at(ifd + 2 + n * 12) as usize;
        }
        assert!(max_ifd_end <= min_tile_offset, "IFDs must precede tile data");
    }
}
