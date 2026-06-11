//! Multiband GeoTIFF reading.
//!
//! surtgis-core's native reader handles single-band rasters only (its
//! `band` parameter is currently a no-op and RGB images fail dimension
//! checks), so geotiles ships its own reader on top of the `tiff` crate:
//! the interleaved buffer is split into one [`Raster<f64>`] per band, and
//! the GeoTIFF tags (transform, CRS, nodata) are attached to each.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use surtgis_core::{CRS, GeoTransform, Raster};
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;

use crate::error::{Error, Result};

const TAG_MODEL_PIXEL_SCALE: u16 = 33550;
const TAG_MODEL_TIEPOINT: u16 = 33922;
const TAG_GEO_KEY_DIRECTORY: u16 = 34735;
const TAG_GDAL_NODATA: u16 = 42113;

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
/// `bands = None` reads every band in order. Each returned raster carries
/// the file's geotransform, CRS and nodata; for float data, nodata cells
/// are additionally normalized to NaN (mirroring surtgis-core).
pub fn read_bands(path: impl AsRef<Path>, bands: Option<&[usize]>) -> Result<Vec<Raster<f64>>> {
    let path = path.as_ref();
    let mut decoder = open(path)?;

    let (width, height) = decoder
        .dimensions()
        .map_err(|e| Error::InvalidInput(format!("cannot read dimensions: {e}")))?;
    let (cols, rows) = (width as usize, height as usize);
    let spp = decoder.get_tag_u32(Tag::SamplesPerPixel).map(|v| v as usize).unwrap_or(1);

    let indices: Vec<usize> = match bands {
        Some(list) if !list.is_empty() => {
            for &b in list {
                if b == 0 || b > spp {
                    return Err(Error::InvalidInput(format!(
                        "band {b} out of range; {} has {spp} band(s)",
                        path.display()
                    )));
                }
            }
            list.iter().map(|&b| b - 1).collect()
        }
        _ => (0..spp).collect(),
    };

    let buf: Vec<f64> = match decoder
        .read_image()
        .map_err(|e| Error::InvalidInput(format!("cannot read image data: {e}")))?
    {
        DecodingResult::U8(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::U16(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::U32(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::I8(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::I16(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::I32(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::F32(v) => v.into_iter().map(f64::from).collect(),
        DecodingResult::F64(v) => v,
        _ => {
            return Err(Error::InvalidInput("unsupported TIFF pixel format".into()));
        }
    };
    if buf.len() != rows * cols * spp {
        return Err(Error::InvalidInput(format!(
            "pixel buffer size mismatch: got {}, expected {}×{}×{spp}",
            buf.len(),
            rows,
            cols
        )));
    }

    let transform = read_geotransform(&mut decoder);
    let crs = read_crs(&mut decoder);
    let nodata = read_nodata(&mut decoder);

    let mut out = Vec::with_capacity(indices.len());
    for &b in &indices {
        let mut data = Vec::with_capacity(rows * cols);
        for px in 0..rows * cols {
            let mut v = buf[px * spp + b];
            // Normalize nodata to NaN so sampling/nodata checks see it.
            if let Some(nd) = nodata
                && v == nd
            {
                v = f64::NAN;
            }
            data.push(v);
        }
        let mut raster = Raster::from_vec(data, rows, cols)?;
        if let Some(t) = transform {
            raster.set_transform(t);
        }
        raster.set_crs(crs.clone());
        raster.set_nodata(nodata);
        out.push(raster);
    }
    Ok(out)
}

fn read_geotransform<R: std::io::Read + std::io::Seek>(
    decoder: &mut Decoder<R>,
) -> Option<GeoTransform> {
    let scale = decoder.get_tag_f64_vec(Tag::Unknown(TAG_MODEL_PIXEL_SCALE)).ok()?;
    let tiepoint = decoder.get_tag_f64_vec(Tag::Unknown(TAG_MODEL_TIEPOINT)).ok()?;
    if scale.len() >= 2 && tiepoint.len() >= 6 {
        // tiepoint: [I, J, K, X, Y, Z]; scale: [Sx, Sy, Sz].
        let origin_x = tiepoint[3] - tiepoint[0] * scale[0];
        let origin_y = tiepoint[4] + tiepoint[1] * scale[1];
        Some(GeoTransform::new(origin_x, origin_y, scale[0], -scale[1]))
    } else {
        None
    }
}

fn read_crs<R: std::io::Read + std::io::Seek>(decoder: &mut Decoder<R>) -> Option<CRS> {
    let keys = decoder.get_tag_u16_vec(Tag::Unknown(TAG_GEO_KEY_DIRECTORY)).ok()?;
    if keys.len() < 4 {
        return None;
    }
    let num_keys = keys[3] as usize;
    for i in 0..num_keys {
        let base = 4 + i * 4;
        if base + 3 >= keys.len() {
            break;
        }
        // GeographicTypeGeoKey (2048) or ProjectedCSTypeGeoKey (3072).
        if (keys[base] == 2048 || keys[base] == 3072) && keys[base + 3] > 0 {
            return Some(CRS::from_epsg(keys[base + 3] as u32));
        }
    }
    None
}

fn read_nodata<R: std::io::Read + std::io::Seek>(decoder: &mut Decoder<R>) -> Option<f64> {
    let s = decoder.get_tag_ascii_string(Tag::Unknown(TAG_GDAL_NODATA)).ok()?;
    s.trim().trim_end_matches('\0').parse().ok()
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
            r.set_transform(GeoTransform::new(-71.0, -33.0, 0.1, -0.1));
            r.set_crs(Some(CRS::from_epsg(4326)));
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
        r.set_transform(GeoTransform::new(-71.0, -33.0, 0.01, -0.01));
        r.set_crs(Some(CRS::from_epsg(4326)));
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
}
