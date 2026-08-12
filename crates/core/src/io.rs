//! Multiband GeoTIFF reading, delegated to surtgis-core v1.0+.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use surtgis_core::GeoTransform;
use surtgis_core::Raster;
use tiff::decoder::{ChunkType, Decoder, DecodingResult, Limits};
use tiff::tags::Tag;

use crate::error::{Error, Result};

fn open(path: &Path) -> Result<Decoder<BufReader<File>>> {
    let file = File::open(path).map_err(|source| Error::Io { path: path.to_path_buf(), source })?;
    Decoder::new(BufReader::new(file))
        .map(|d| d.with_limits(Limits::unlimited()))
        .map_err(|e| Error::InvalidInput(format!("{}: TIFF decode error: {e}", path.display())))
}

/// Number of samples per pixel (bands) in the first IFD.
pub fn band_count(path: impl AsRef<Path>) -> Result<usize> {
    let mut decoder = open(path.as_ref())?;
    Ok(decoder
        .get_tag_u32(Tag::SamplesPerPixel)
        .map(|v| v as usize)
        .unwrap_or(1))
}

/// Read selected bands (1-based indices) of a GeoTIFF as `f64` rasters.
///
/// Delegates to surtgis-core's native multi-band reader. `bands = None`
/// reads every band in order. Each returned raster carries the file's
/// geotransform, CRS and nodata (normalized to NaN).
pub fn read_bands(path: impl AsRef<Path>, bands: Option<&[usize]>) -> Result<Vec<Raster<f64>>> {
    let path = path.as_ref();
    let all_bands: Vec<Raster<f64>> = surtgis_core::io::read_geotiff_bands(path)?;

    let indices: Vec<usize> = match bands {
        Some(list) if !list.is_empty() => {
            for &b in list {
                if b == 0 || b > all_bands.len() {
                    return Err(Error::InvalidInput(format!(
                        "band {b} out of range; {} has {} band(s)",
                        path.display(),
                        all_bands.len()
                    )));
                }
            }
            list.iter().map(|&b| b - 1).collect()
        }
        _ => (0..all_bands.len()).collect(),
    };

    Ok(indices.into_iter().map(|i| all_bands[i].clone()).collect())
}

/// Scan a band of a GeoTIFF for its finite data range `(min, max)`,
/// reading it window-by-window so memory stays bounded.
///
/// Returns `(0.0, 1.0)` for a band with no finite values.
pub fn band_minmax(path: impl AsRef<Path>, band: usize) -> Result<(f64, f64)> {
    let path = path.as_ref();
    let meta = read_meta(path, &[band])?;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;

    // Window the scan in ~1024-pixel tiles of source pixels.
    const STEP: i64 = 1024;
    let mut row0 = 0i64;
    while row0 < meta.rows as i64 {
        let mut col0 = 0i64;
        while col0 < meta.cols as i64 {
            let win = read_band_window(path, band, col0, row0, STEP as usize, STEP as usize)?;
            let (rows, cols) = win.shape();
            for i in 0..rows {
                for j in 0..cols {
                    let v = win.get(i, j).ok();
                    if let Some(v) = v
                        && v.is_finite()
                    {
                        if v < min {
                            min = v;
                        }
                        if v > max {
                            max = v;
                        }
                    }
                }
            }
            col0 += STEP;
        }
        row0 += STEP;
    }

    if !min.is_finite() || !max.is_finite() {
        return Ok((0.0, 1.0));
    }
    Ok((min, max))
}

/// Metadata shared by the windowed readers: everything needed to tile a
/// file without loading its pixel data.
pub struct WindowedMeta {
    pub rows: usize,
    pub cols: usize,
    pub transform: GeoTransform,
    pub crs: Option<surtgis_core::CRS>,
    pub nodata: Option<f64>,
    /// 0-based source band indices this reader serves.
    pub bands: Vec<usize>,
}

/// Read only the header/geo metadata of a GeoTIFF (no pixel data).
pub fn read_meta(path: impl AsRef<Path>, bands: &[usize]) -> Result<WindowedMeta> {
    let mut decoder = open(path.as_ref())?;
    let (cols, rows) = decoder
        .dimensions()
        .map_err(|e| Error::InvalidInput(format!("{}: {e}", path.as_ref().display())))?;
    let transform = read_transform(&mut decoder)?;
    let crs = read_crs(&mut decoder);
    let nodata = read_nodata(&mut decoder);
    Ok(WindowedMeta {
        rows: rows as usize,
        cols: cols as usize,
        transform,
        crs,
        nodata,
        bands: bands.to_vec(),
    })
}

/// Read one 0-based band of a sub-rectangle `(col0, row0)` sized
/// `(width, height)` from a GeoTIFF, decoding only the chunks (strips or
/// tiles) that intersect the window.
///
/// The returned raster carries the file's CRS and nodata (→ NaN), and a
/// geotransform shifted so that (col0, row0) is its top-left corner —
/// the same geo coordinates map to pixel (0, 0) of the window. The window
/// is clamped to the raster: a request that pokes outside returns the
/// valid intersection, possibly smaller than requested.
pub fn read_band_window(
    path: impl AsRef<Path>,
    band: usize,
    col0: i64,
    row0: i64,
    width: usize,
    height: usize,
) -> Result<Raster<f64>> {
    let path = path.as_ref();
    let mut decoder = open(path)?;
    let (cols, rows) = decoder
        .dimensions()
        .map_err(|e| Error::InvalidInput(format!("{}: {e}", path.display())))?;
    let spp = decoder
        .get_tag_u32(Tag::SamplesPerPixel)
        .map(|v| v as usize)
        .unwrap_or(1);
    let transform = read_transform(&mut decoder)?;
    let crs = read_crs(&mut decoder);
    let nodata = read_nodata(&mut decoder);

    // Clamp the window to the raster extent.
    let col_start = col0.clamp(0, cols as i64);
    let row_start = row0.clamp(0, rows as i64);
    let col_end = (col0 + width as i64).clamp(0, cols as i64);
    let row_end = (row0 + height as i64).clamp(0, rows as i64);
    let w = (col_end - col_start) as usize;
    let h = (row_end - row_start) as usize;
    if w == 0 || h == 0 {
        return Err(Error::InvalidInput(format!(
            "window ({col0},{row0}) {width}x{height} does not intersect {}",
            path.display()
        )));
    }

    let chunk_type = decoder.get_chunk_type();
    let (chunk_w, chunk_h) = decoder.chunk_dimensions();
    let (chunk_w, chunk_h) = (chunk_w as usize, chunk_h as usize);
    let tiles_across = (cols as usize).div_ceil(chunk_w);

    let mut data = vec![f64::NAN; w * h];

    // Row/col ranges (in source pixels) covered by the window.
    let (c0, r0) = (col_start as usize, row_start as usize);
    let (c1, r1) = (col_end as usize, row_end as usize);

    // Iterate over the chunks intersecting the window, reading each once.
    let (first_c, first_r) = (c0 / chunk_w, r0 / chunk_h);
    let (last_c, last_r) = (c1.div_ceil(chunk_w), r1.div_ceil(chunk_h));
    for tr in first_r..last_r {
        for tc in first_c..last_c {
            let chunk_idx = match chunk_type {
                ChunkType::Tile => tr * tiles_across + tc,
                ChunkType::Strip => {
                    // Strips span the full width; tc is meaningless.
                    let _ = tc;
                    tr
                }
            };
            let (cdw, cdh) = decoder.chunk_data_dimensions(chunk_idx as u32);
            let (cdw, cdh) = (cdw as usize, cdh as usize);

            let result = decoder
                .read_chunk(chunk_idx as u32)
                .map_err(|e| Error::InvalidInput(format!("{}: {e}", path.display())))?;

            // Chunk's top-left source pixel.
            let (bc, br) = (tc * chunk_w, tr * chunk_h);
            // Intersection of [br, br+cdh) x [bc, bc+cdw) with the window.
            let x0 = bc.max(c0);
            let y0 = br.max(r0);
            let x1 = (bc + cdw).min(c1);
            let y1 = (br + cdh).min(r1);

            for (sy, y) in (y0..y1).enumerate() {
                for (sx, x) in (x0..x1).enumerate() {
                    let local_c = x - bc;
                    let local_r = y - br;
                    let idx = (local_r * cdw + local_c) * spp + band;
                    let v = result_to_f64(&result, idx);
                    let v = if nodata.is_some_and(|nd| approx_nodata(v, nd)) {
                        f64::NAN
                    } else {
                        v
                    };
                    let dst = (y - r0) * w + (x - c0);
                    data[dst] = v;
                    let _ = (sx, sy);
                }
            }
        }
    }

    // Shifted geotransform: window top-left corner in geo coordinates.
    let origin_x = transform.origin_x + col_start as f64 * transform.pixel_width;
    let origin_y = transform.origin_y + row_start as f64 * transform.pixel_height;
    let shifted = GeoTransform::new(origin_x, origin_y, transform.pixel_width, transform.pixel_height);

    let mut raster = Raster::from_vec(data, h, w)?;
    raster.set_transform(shifted);
    if let Some(crs) = crs {
        raster.set_crs(Some(crs));
    }
    Ok(raster)
}

/// Convert one interleaved sample from a decoded chunk to f64.
fn result_to_f64(result: &DecodingResult, idx: usize) -> f64 {
    match result {
        DecodingResult::F64(v) => v[idx],
        DecodingResult::F32(v) => v[idx] as f64,
        DecodingResult::U8(v) => v[idx] as f64,
        DecodingResult::U16(v) => v[idx] as f64,
        DecodingResult::U32(v) => v[idx] as f64,
        DecodingResult::U64(v) => v[idx] as f64,
        DecodingResult::I8(v) => v[idx] as f64,
        DecodingResult::I16(v) => v[idx] as f64,
        DecodingResult::I32(v) => v[idx] as f64,
        DecodingResult::I64(v) => v[idx] as f64,
        DecodingResult::F16(_) => f64::NAN,
    }
}

fn approx_nodata(v: f64, nd: f64) -> bool {
    v == nd || (v.is_nan() && nd.is_nan())
}

/// Read the affine geotransform from ModelPixelScale + ModelTiepoint.
fn read_transform<R: std::io::Read + std::io::Seek>(decoder: &mut Decoder<R>) -> Result<GeoTransform> {
    let scale = decoder
        .get_tag_f64_vec(Tag::Unknown(33550))
        .map_err(|_| Error::InvalidInput("no ModelPixelScale tag".into()))?;
    let tiepoint = decoder
        .get_tag_f64_vec(Tag::Unknown(33922))
        .map_err(|_| Error::InvalidInput("no ModelTiepoint tag".into()))?;
    if scale.len() >= 2 && tiepoint.len() >= 6 {
        let origin_x = tiepoint[3] - tiepoint[0] * scale[0];
        let origin_y = tiepoint[4] + tiepoint[1] * scale[1];
        return Ok(GeoTransform::new(origin_x, origin_y, scale[0], -scale[1]));
    }
    Err(Error::InvalidInput("malformed ModelPixelScale/ModelTiepoint".into()))
}

/// Read the EPSG code from GeoKeyDirectory (2048/3072 keys).
fn read_crs<R: std::io::Read + std::io::Seek>(decoder: &mut Decoder<R>) -> Option<surtgis_core::CRS> {
    let geokeys = decoder.get_tag_u16_vec(Tag::Unknown(34735)).ok()?;
    if geokeys.len() < 4 {
        return None;
    }
    let n = geokeys[3] as usize;
    for i in 0..n {
        let base = 4 + i * 4;
        if base + 3 >= geokeys.len() {
            break;
        }
        let key = geokeys[base];
        let value = geokeys[base + 3];
        if (key == 3072 || key == 2048) && value > 0 {
            return Some(surtgis_core::CRS::from_epsg(value as u32));
        }
    }
    None
}

/// Read GDAL_NODATA (42113), stored as ASCII (or legacy BYTE) string.
fn read_nodata<R: std::io::Read + std::io::Seek>(decoder: &mut Decoder<R>) -> Option<f64> {
    let s = match decoder.get_tag_ascii_string(Tag::Unknown(42113)) {
        Ok(s) => s,
        Err(_) => {
            let bytes = decoder.get_tag_u8_vec(Tag::Unknown(42113)).ok()?;
            String::from_utf8_lossy(&bytes).into_owned()
        }
    };
    s.trim().trim_end_matches('\0').parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an RGB float GeoTIFF via surtgis' multiband writer, then read
    /// it back band by band.
    #[test]
    fn rgb_roundtrip_via_surtgis_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rgb.tif");

        let mut bands = vec![];
        for b in 0..3usize {
            let mut r = Raster::new(2, 3);
            for i in 0..2 {
                for j in 0..3 {
                    r.set(i, j, (b * 100 + i * 10 + j) as f64).unwrap();
                }
            }
            r.set_transform(surtgis_core::GeoTransform::new(-71.0, -33.0, 0.1, -0.1));
            r.set_crs(Some(surtgis_core::CRS::from_epsg(4326)));
            bands.push(r);
        }
        let refs: Vec<&Raster<f64>> = bands.iter().collect();
        surtgis_core::io::write_geotiff_multiband(&refs, &path, None).unwrap();

        assert_eq!(band_count(&path).unwrap(), 3);

        let back = read_bands(&path, None).unwrap();
        assert_eq!(back.len(), 3);
        for (b, r) in back.iter().enumerate() {
            assert_eq!(r.shape(), (2, 3));
            assert_eq!(r.get(1, 2).unwrap(), (b * 100 + 12) as f64);
        }
        // Geo metadata survives.
        assert_eq!(back[0].crs().and_then(|c| c.epsg()), Some(4326));
        assert!((back[0].cell_size() - 0.1).abs() < 1e-12);

        // Band selection (1-based, out of order).
        let sel = read_bands(&path, Some(&[3, 1])).unwrap();
        assert_eq!(sel[0].get(0, 0).unwrap(), 200.0);
        assert_eq!(sel[1].get(0, 0).unwrap(), 0.0);

        // Out-of-range band rejected.
        assert!(read_bands(&path, Some(&[4])).is_err());
    }

    /// Our own COG output must round-trip through this reader too.
    #[test]
    fn reads_geotiles_cog_output() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.tif");
        let mut r = Raster::new(20, 20);
        for i in 0..20 {
            for j in 0..20 {
                r.set(i, j, (i + j) as f64).unwrap();
            }
        }
        r.set_transform(surtgis_core::GeoTransform::new(-71.0, -33.0, 0.01, -0.01));
        r.set_crs(Some(surtgis_core::CRS::from_epsg(4326)));
        crate::cog::write_cog(&r, &path, &crate::cog::CogOptions {
            tile_size: 16,
            ..Default::default()
        })
        .unwrap();

        let back = read_bands(&path, None).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].shape(), (20, 20));
        assert_eq!(back[0].get(5, 7).unwrap(), 12.0);
        assert_eq!(back[0].crs().and_then(|c| c.epsg()), Some(4326));
    }

    fn window_source(rows: usize, cols: usize) -> Raster<f64> {
        let mut r = Raster::new(rows, cols);
        for i in 0..rows {
            for j in 0..cols {
                r.set(i, j, (i * 1000 + j) as f64).unwrap();
            }
        }
        r.set_transform(surtgis_core::GeoTransform::new(
            -71.0,
            -33.0,
            0.01,
            -0.01,
        ));
        r.set_crs(Some(surtgis_core::CRS::from_epsg(4326)));
        r
    }

    /// A window read must match the corresponding cells of the full-RAM
    /// read, for a tiled (COG) file whose tiles are smaller than the window.
    #[test]
    fn band_window_matches_full_read_tiled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("w.tif");
        let r = window_source(40, 60);
        crate::cog::write_cog(&r, &path, &crate::cog::CogOptions {
            tile_size: 16,
            ..Default::default()
        })
        .unwrap();

        // Window overlapping 4 tiles (c 10..34, r 5..25).
        let win = read_band_window(&path, 0, 10, 5, 24, 20).unwrap();
        assert_eq!(win.shape(), (20, 24));

        let full = &read_bands(&path, None).unwrap()[0];
        for i in 0..20 {
            for j in 0..24 {
                assert_eq!(win.get(i, j).unwrap(), full.get(5 + i, 10 + j).unwrap());
            }
        }
        // CRS survives.
        assert_eq!(win.crs().and_then(|c| c.epsg()), Some(4326));
    }

    /// Window clipped to the raster edge: request past the boundary must
    /// return the valid intersection.
    #[test]
    fn band_window_clips_at_edge_tiled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edge.tif");
        let r = window_source(20, 20);
        crate::cog::write_cog(&r, &path, &crate::cog::CogOptions {
            tile_size: 16,
            ..Default::default()
        })
        .unwrap();

        // Requests a 12x12 window at (15,15): only 5x5 remains.
        let win = read_band_window(&path, 0, 15, 15, 12, 12).unwrap();
        assert_eq!(win.shape(), (5, 5));
        let full = &read_bands(&path, None).unwrap()[0];
        assert_eq!(win.get(0, 0).unwrap(), full.get(15, 15).unwrap());
        assert_eq!(win.get(4, 4).unwrap(), full.get(19, 19).unwrap());

        // Fully outside → error.
        assert!(read_band_window(&path, 0, 25, 25, 4, 4).is_err());
    }

    /// The same, for a stripped (non-tiled) GeoTIFF written by surtgis.
    #[test]
    fn band_window_matches_full_read_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.tif");
        let r = window_source(30, 30);
        surtgis_core::io::write_geotiff(&r, &path, None).unwrap();

        let win = read_band_window(&path, 0, 8, 4, 15, 15).unwrap();
        assert_eq!(win.shape(), (15, 15));
        let full = &read_bands(&path, None).unwrap()[0];
        for i in 0..15 {
            for j in 0..15 {
                assert_eq!(win.get(i, j).unwrap(), full.get(4 + i, 8 + j).unwrap());
            }
        }
    }

    /// Meta-only read exposes shape/CRS without touching pixel data.
    #[test]
    fn meta_read_reports_shape_and_crs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.tif");
        let r = window_source(12, 14);
        crate::cog::write_cog(&r, &path, &crate::cog::CogOptions::default()).unwrap();

        let meta = read_meta(&path, &[0]).unwrap();
        assert_eq!((meta.rows, meta.cols), (12, 14));
        assert_eq!(meta.crs.as_ref().and_then(|c| c.epsg()), Some(4326));
        assert_eq!(meta.bands, vec![0]);
        assert!((meta.transform.cell_size() - 0.01).abs() < 1e-12);
    }
}
