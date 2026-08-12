//! Raster source abstraction: CRS handling and resampling.
//!
//! v0.1 supports sources in EPSG:4326 (lon/lat) and EPSG:3857 (Web Mercator).
//! Reprojection between those and the tile grid is analytic, so no external
//! projection engine is needed.
//!
//! v0.4 adds a file-backed source: a GeoTIFF is opened for its metadata and
//! each tile renders against the *window* of source pixels it needs, read on
//! demand (see [`RasterSource::window_source`]). Memory scales with the tile
//! window instead of the whole raster.

use std::path::{Path, PathBuf};

use surtgis_core::GeoTransform;
use surtgis_core::Raster;

use crate::error::{Error, Result};
use crate::mercator::{self, TileCoord, MAX_LATITUDE_DEG, ORIGIN_SHIFT_M};

/// Coordinate system of the source raster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceCrs {
    /// Geographic lon/lat degrees (EPSG:4326).
    LonLat,
    /// Spherical Web Mercator meters (EPSG:3857 / 900913).
    Mercator,
}

/// Resampling method used when reading source pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Resampling {
    /// Nearest neighbour — categorical data, fastest.
    Nearest,
    /// Bilinear over valid (non-nodata) neighbours — continuous data.
    #[default]
    Bilinear,
}

/// Where a source's pixel data lives.
#[derive(Debug)]
enum SourceData {
    /// Bands fully decoded in memory.
    Memory(Vec<Raster<f64>>),
    /// GeoTIFF on disk: metadata only, pixels read window-by-window.
    File(FileSource),
}

/// Metadata for a file-backed source; no pixel data.
#[derive(Debug)]
struct FileSource {
    path: PathBuf,
    /// 0-based source band indices served.
    bands: Vec<usize>,
    rows: usize,
    cols: usize,
    transform: GeoTransform,
}

/// A tileable raster: 1 (gray), 3 (RGB) or 4 (RGBA) co-registered bands
/// plus the analytic projection to Web Mercator.
#[derive(Debug)]
pub struct RasterSource {
    data: SourceData,
    crs: SourceCrs,
}

impl RasterSource {
    /// Wrap a single-band raster, detecting its CRS.
    ///
    /// Detection order: explicit `crs_override`, then the raster's EPSG code
    /// (4326 → lon/lat, 3857/900913 → mercator), then a bounds heuristic
    /// (coordinates within ±180/±90 look like degrees). Anything else is
    /// rejected — reproject the input first.
    pub fn new(raster: Raster<f64>, crs_override: Option<SourceCrs>) -> Result<Self> {
        Self::new_multi(vec![raster], crs_override)
    }

    /// Wrap 1, 3 or 4 co-registered bands (gray, RGB, RGBA).
    ///
    /// All bands must share shape and geotransform; CRS detection follows
    /// the rules of [`RasterSource::new`] using the first band.
    pub fn new_multi(bands: Vec<Raster<f64>>, crs_override: Option<SourceCrs>) -> Result<Self> {
        if !matches!(bands.len(), 1 | 3 | 4) {
            return Err(Error::InvalidInput(format!(
                "expected 1, 3 or 4 bands, got {}",
                bands.len()
            )));
        }
        let first = &bands[0];
        if first.rows() == 0 || first.cols() == 0 {
            return Err(Error::InvalidInput("empty raster".into()));
        }
        if !first.transform().is_north_up() {
            return Err(Error::InvalidInput(
                "rotated rasters are not supported; warp to north-up first".into(),
            ));
        }
        for (i, b) in bands.iter().enumerate().skip(1) {
            if b.shape() != first.shape() {
                return Err(Error::InvalidInput(format!(
                    "band {} shape {:?} differs from band 1 {:?}",
                    i + 1,
                    b.shape(),
                    first.shape()
                )));
            }
            let (a, c) = (b.transform().to_gdal(), first.transform().to_gdal());
            if a.iter().zip(&c).any(|(x, y)| (x - y).abs() > 1e-9) {
                return Err(Error::InvalidInput(format!(
                    "band {} geotransform differs from band 1",
                    i + 1
                )));
            }
        }

        let crs = match crs_override {
            Some(crs) => crs,
            None => Self::detect_crs(first)?,
        };
        Ok(Self { data: SourceData::Memory(bands), crs })
    }

    /// Open a GeoTIFF for streaming tiling without loading its pixels.
    ///
    /// Only the header is read; [`RasterSource::window_source`] reads the
    /// pixels each tile needs. `bands` are 0-based source band indices.
    pub fn open_file(
        path: impl AsRef<Path>,
        bands: &[usize],
        crs_override: Option<SourceCrs>,
    ) -> Result<Self> {
        let path = path.as_ref();
        if !matches!(bands.len(), 1 | 3 | 4) {
            return Err(Error::InvalidInput(format!(
                "expected 1, 3 or 4 bands, got {}",
                bands.len()
            )));
        }
        let meta = crate::io::read_meta(path, bands)?;
        if !meta.transform.is_north_up() {
            return Err(Error::InvalidInput(
                "rotated rasters are not supported; warp to north-up first".into(),
            ));
        }
        let crs = match crs_override {
            Some(crs) => crs,
            None => match meta.crs.as_ref().and_then(|c| c.epsg()) {
                Some(4326) => SourceCrs::LonLat,
                Some(3857) | Some(900_913) => SourceCrs::Mercator,
                Some(other) => {
                    return Err(Error::InvalidInput(format!(
                        "unsupported source CRS EPSG:{other}; reproject to EPSG:4326 or \
                         EPSG:3857 first, or pass an explicit --source-crs override"
                    )));
                }
                None => {
                    // Bounds heuristic, mirroring detect_crs.
                    let looks_geographic = meta.transform.origin_x >= -180.5
                        && meta.transform.origin_x + meta.cols as f64 * meta.transform.pixel_width
                            <= 180.5;
                    if looks_geographic {
                        SourceCrs::LonLat
                    } else {
                        SourceCrs::Mercator
                    }
                }
            },
        };
        Ok(Self {
            data: SourceData::File(FileSource {
                path: path.to_path_buf(),
                bands: bands.to_vec(),
                rows: meta.rows,
                cols: meta.cols,
                transform: meta.transform,
            }),
            crs,
        })
    }

    /// Number of bands (1, 3 or 4).
    pub fn band_count(&self) -> usize {
        match &self.data {
            SourceData::Memory(bands) => bands.len(),
            SourceData::File(f) => f.bands.len(),
        }
    }

    /// Whether pixels are read from disk per tile rather than held in RAM.
    pub fn is_file_backed(&self) -> bool {
        matches!(self.data, SourceData::File(_))
    }

    fn detect_crs(raster: &Raster<f64>) -> Result<SourceCrs> {
        if let Some(code) = raster.crs().and_then(|c| c.epsg()) {
            return match code {
                4326 => Ok(SourceCrs::LonLat),
                3857 | 900_913 => Ok(SourceCrs::Mercator),
                other => Err(Error::InvalidInput(format!(
                    "unsupported source CRS EPSG:{other}; reproject to EPSG:4326 or \
                     EPSG:3857 first, or pass an explicit --source-crs override"
                ))),
            };
        }
        // No CRS metadata: fall back to a bounds heuristic.
        let (min_x, min_y, max_x, max_y) = raster.bounds();
        let looks_geographic = min_x >= -180.5 && max_x <= 180.5 && min_y >= -90.5 && max_y <= 90.5;
        let looks_mercator = min_x.abs() <= ORIGIN_SHIFT_M * 1.001
            && max_x.abs() <= ORIGIN_SHIFT_M * 1.001
            && (max_x - min_x) > 360.0;
        if looks_geographic {
            Ok(SourceCrs::LonLat)
        } else if looks_mercator {
            Ok(SourceCrs::Mercator)
        } else {
            Err(Error::InvalidInput(
                "source has no CRS metadata and bounds are ambiguous; pass an explicit \
                 CRS override"
                    .into(),
            ))
        }
    }

    /// The detected or overridden source CRS.
    pub fn crs(&self) -> SourceCrs {
        self.crs
    }

    /// Borrow the first band (the only band for single-band sources).
    ///
    /// Only available for in-memory sources.
    pub fn raster(&self) -> &Raster<f64> {
        let SourceData::Memory(bands) = &self.data else {
            unreachable!("raster() requires an in-memory source; use window_source");
        };
        &bands[0]
    }

    /// Borrow a band by 0-based index.
    ///
    /// Only available for in-memory sources.
    pub fn band(&self, b: usize) -> &Raster<f64> {
        let SourceData::Memory(bands) = &self.data else {
            unreachable!("band() requires an in-memory source; use window_source");
        };
        &bands[b]
    }

    /// Access a band if the source is in memory.
    fn band_opt(&self, b: usize) -> Option<&Raster<f64>> {
        let SourceData::Memory(bands) = &self.data else {
            return None;
        };
        bands.get(b)
    }

    /// The source pixel window `(col0, row0, width, height)` that `coord`
    /// maps onto, clamped to the raster. `None` if the tile doesn't
    /// intersect the source or the source is in memory.
    pub fn window_rect(&self, coord: TileCoord) -> Option<(i64, i64, usize, usize)> {
        let SourceData::File(f) = &self.data else {
            return None;
        };
        let (min_mx, min_my, max_mx, max_my) = coord.bounds_meters();
        let ((sx0, sy0), (sx1, sy1)) = match self.crs {
            SourceCrs::Mercator => ((min_mx, max_my), (max_mx, min_my)),
            SourceCrs::LonLat => (
                mercator::meters_to_lonlat(min_mx, max_my),
                mercator::meters_to_lonlat(max_mx, min_my),
            ),
        };
        let (ca, ra) = f.transform.geo_to_pixel(sx0, sy0);
        let (cb, rb) = f.transform.geo_to_pixel(sx1, sy1);
        if !ca.is_finite() || !cb.is_finite() || !ra.is_finite() || !rb.is_finite() {
            return None;
        }
        const MARGIN: i64 = 2;
        let col0 = ((ca.min(cb)).floor() as i64 - MARGIN).max(0);
        let row0 = ((ra.min(rb)).floor() as i64 - MARGIN).max(0);
        let col1 = ((ca.max(cb)).ceil() as i64 + MARGIN).min(f.cols as i64);
        let row1 = ((ra.max(rb)).ceil() as i64 + MARGIN).min(f.rows as i64);
        let width = (col1 - col0) as usize;
        let height = (row1 - row0) as usize;
        if width == 0 || height == 0 {
            return None;
        }
        Some((col0, row0, width, height))
    }

    /// Estimated in-memory bytes of the window `coord` needs (file-backed).
    ///
    /// Used by the renderer to bound total in-flight window memory.
    pub fn window_bytes(&self, coord: TileCoord) -> Option<usize> {
        let (_, _, width, height) = self.window_rect(coord)?;
        Some(width * height * self.band_count() * size_of::<f64>())
    }

    /// Materialise the window of source pixels that `coord` needs, as an
    /// in-memory source whose top-left pixel is the window's top-left.
    ///
    /// File-backed sources call this once per tile; in-memory sources
    /// return `None` (they are already fully available).
    pub fn window_source(&self, coord: TileCoord) -> Result<Option<RasterSource>> {
        let SourceData::File(f) = &self.data else {
            return Ok(None);
        };
        let Some((col0, row0, width, height)) = self.window_rect(coord) else {
            return Ok(Some(RasterSource {
                data: SourceData::Memory(vec![]),
                crs: self.crs,
            }));
        };

        let mut window_bands = Vec::with_capacity(f.bands.len());
        for &band in &f.bands {
            window_bands.push(crate::io::read_band_window(
                &f.path,
                band,
                col0,
                row0,
                width,
                height,
            )?);
        }
        let source = Self {
            data: SourceData::Memory(window_bands),
            crs: self.crs,
        };
        Ok(Some(source))
    }

    /// Source bounds in Web Mercator meters `(min_x, min_y, max_x, max_y)`.
    pub fn bounds_meters(&self) -> (f64, f64, f64, f64) {
        let (min_x, min_y, max_x, max_y) = match &self.data {
            SourceData::Memory(bands) => bands[0].bounds(),
            SourceData::File(f) => {
                let (x0, y0) = f.transform.pixel_to_geo_corner(0, 0);
                let (x1, y1) =
                    f.transform.pixel_to_geo_corner(f.cols, f.rows);
                (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
            }
        };
        match self.crs {
            SourceCrs::Mercator => (min_x, min_y, max_x, max_y),
            SourceCrs::LonLat => {
                let south = min_y.clamp(-MAX_LATITUDE_DEG, MAX_LATITUDE_DEG);
                let north = max_y.clamp(-MAX_LATITUDE_DEG, MAX_LATITUDE_DEG);
                let (mx0, my0) = mercator::lonlat_to_meters(min_x.max(-180.0), south);
                let (mx1, my1) = mercator::lonlat_to_meters(max_x.min(180.0), north);
                (mx0, my0, mx1, my1)
            }
        }
    }

    /// Source bounds in lon/lat degrees `(west, south, east, north)`.
    pub fn bounds_lonlat(&self) -> (f64, f64, f64, f64) {
        let (min_x, min_y, max_x, max_y) = match &self.data {
            SourceData::Memory(bands) => bands[0].bounds(),
            SourceData::File(f) => {
                let (x0, y0) = f.transform.pixel_to_geo_corner(0, 0);
                let (x1, y1) = f.transform.pixel_to_geo_corner(f.cols, f.rows);
                (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
            }
        };
        match self.crs {
            SourceCrs::LonLat => (min_x, min_y, max_x, max_y),
            SourceCrs::Mercator => {
                let (west, south) = mercator::meters_to_lonlat(min_x, min_y);
                let (east, north) = mercator::meters_to_lonlat(max_x, max_y);
                (west, south, east, north)
            }
        }
    }

    /// Approximate native resolution in Web Mercator meters/pixel.
    ///
    /// For lon/lat sources this uses the equatorial conversion
    /// (deg × πR/180), matching gdal2tiles' zoom selection.
    pub fn native_resolution_m(&self) -> f64 {
        let cell = match &self.data {
            SourceData::Memory(bands) => bands[0].cell_size(),
            SourceData::File(f) => f.transform.cell_size(),
        };
        match self.crs {
            SourceCrs::Mercator => cell,
            SourceCrs::LonLat => cell * ORIGIN_SHIFT_M / 180.0,
        }
    }

    /// Natural maximum zoom: tiling deeper than this adds no detail.
    pub fn native_max_zoom(&self, tile_size: u32) -> u8 {
        mercator::zoom_for_resolution(self.native_resolution_m(), tile_size)
    }

    /// Data range `(min, max)` of the first band's finite values.
    ///
    /// File-backed sources scan the file window-by-window so memory stays
    /// bounded; in-memory sources read the band directly.
    pub fn minmax(&self) -> Result<(f64, f64)> {
        match &self.data {
            SourceData::Memory(bands) => {
                let raster = &bands[0];
                let mut min = f64::INFINITY;
                let mut max = f64::NEG_INFINITY;
                for val in raster.data().iter() {
                    if val.is_nan() || raster.is_nodata(*val) {
                        continue;
                    }
                    if *val < min {
                        min = *val;
                    }
                    if *val > max {
                        max = *val;
                    }
                }
                if !min.is_finite() || !max.is_finite() {
                    Ok((0.0, 1.0))
                } else if (max - min).abs() < f64::EPSILON {
                    Ok((min, min + 1.0))
                } else {
                    Ok((min, max))
                }
            }
            SourceData::File(f) => {
                let (min, max) = crate::io::band_minmax(&f.path, f.bands[0])?;
                if (max - min).abs() < f64::EPSILON {
                    Ok((min, min + 1.0))
                } else {
                    Ok((min, max))
                }
            }
        }
    }

    /// Sample the first band at a Web Mercator point. `None` means outside
    /// the raster or nodata.
    pub fn sample(&self, mx: f64, my: f64, method: Resampling) -> Option<f64> {
        self.sample_band(0, mx, my, method)
    }

    /// Sample one band (0-based) at a Web Mercator point.
    pub fn sample_band(&self, band: usize, mx: f64, my: f64, method: Resampling) -> Option<f64> {
        let (sx, sy) = match self.crs {
            SourceCrs::Mercator => (mx, my),
            SourceCrs::LonLat => mercator::meters_to_lonlat(mx, my),
        };
        let (col, row) = self.band_opt(band)?.geo_to_pixel(sx, sy);
        if !col.is_finite() || !row.is_finite() {
            return None;
        }
        match method {
            Resampling::Nearest => self.sample_nearest(band, col, row),
            Resampling::Bilinear => self.sample_bilinear(band, col, row),
        }
    }

    /// Average the valid cells whose centers fall inside a Web Mercator
    /// rectangle `(mx0, my0, mx1, my1)`.
    ///
    /// This is the area-average used for overview zooms, where one output
    /// pixel covers many source cells and point sampling would miss data.
    /// Falls back to bilinear at the rectangle center when the footprint is
    /// smaller than one cell.
    pub fn sample_area(&self, mx0: f64, my0: f64, mx1: f64, my1: f64) -> Option<f64> {
        self.sample_area_band(0, mx0, my0, mx1, my1)
    }

    /// Area-average one band (0-based) over a Web Mercator rectangle.
    pub fn sample_area_band(
        &self,
        band: usize,
        mx0: f64,
        my0: f64,
        mx1: f64,
        my1: f64,
    ) -> Option<f64> {
        let ((sx0, sy0), (sx1, sy1)) = match self.crs {
            SourceCrs::Mercator => ((mx0, my0), (mx1, my1)),
            SourceCrs::LonLat => (
                mercator::meters_to_lonlat(mx0, my0),
                mercator::meters_to_lonlat(mx1, my1),
            ),
        };
        let (ca, ra) = self.band_opt(band)?.geo_to_pixel(sx0.min(sx1), sy0.max(sy1));
        let (cb, rb) = self.band_opt(band)?.geo_to_pixel(sx0.max(sx1), sy0.min(sy1));
        if !ca.is_finite() || !cb.is_finite() || !ra.is_finite() || !rb.is_finite() {
            return None;
        }
        // Cell centers sit at fractional +0.5: center of cell c is inside
        // [ca, cb] iff c ∈ [ceil(ca - 0.5), floor(cb - 0.5)].
        let col_start = (ca.min(cb) - 0.5).ceil() as i64;
        let col_end = (ca.max(cb) - 0.5).floor() as i64;
        let row_start = (ra.min(rb) - 0.5).ceil() as i64;
        let row_end = (ra.max(rb) - 0.5).floor() as i64;

        if col_start > col_end || row_start > row_end {
            // Footprint narrower than a cell: behave like point sampling.
            return self.sample_band(
                band,
                (mx0 + mx1) / 2.0,
                (my0 + my1) / 2.0,
                Resampling::Bilinear,
            );
        }

        // Intersect with the raster so footprints that poke outside don't
        // iterate over (possibly millions of) nonexistent cells.
        let raster = self.band_opt(band)?;
        let col_start = col_start.max(0);
        let col_end = col_end.min(raster.cols() as i64 - 1);
        let row_start = row_start.max(0);
        let row_end = row_end.min(raster.rows() as i64 - 1);

        let mut acc = 0.0;
        let mut count = 0u64;
        for row in row_start..=row_end {
            for col in col_start..=col_end {
                if let Some(v) = self.valid_at(band, row, col) {
                    acc += v;
                    count += 1;
                }
            }
        }
        (count > 0).then(|| acc / count as f64)
    }

    fn valid_at(&self, band: usize, row: i64, col: i64) -> Option<f64> {
        let raster = self.band_opt(band)?;
        if row < 0 || col < 0 || row >= raster.rows() as i64 || col >= raster.cols() as i64 {
            return None;
        }
        let v = raster.get(row as usize, col as usize).ok()?;
        if v.is_nan() || raster.is_nodata(v) {
            None
        } else {
            Some(v)
        }
    }

    fn sample_nearest(&self, band: usize, col: f64, row: f64) -> Option<f64> {
        self.valid_at(band, row.floor() as i64, col.floor() as i64)
    }

    /// Bilinear interpolation between pixel centers, renormalizing weights
    /// over the valid neighbours so nodata does not bleed into the result.
    fn sample_bilinear(&self, band: usize, col: f64, row: f64) -> Option<f64> {
        // geo_to_pixel puts pixel centers at fractional +0.5.
        let u = col - 0.5;
        let v = row - 0.5;
        let c0 = u.floor();
        let r0 = v.floor();
        let fu = u - c0;
        let fv = v - r0;
        let (c0, r0) = (c0 as i64, r0 as i64);

        let neighbours = [
            (r0, c0, (1.0 - fu) * (1.0 - fv)),
            (r0, c0 + 1, fu * (1.0 - fv)),
            (r0 + 1, c0, (1.0 - fu) * fv),
            (r0 + 1, c0 + 1, fu * fv),
        ];

        let mut acc = 0.0;
        let mut wsum = 0.0;
        for (r, c, w) in neighbours {
            if w <= 0.0 {
                continue;
            }
            if let Some(val) = self.valid_at(band, r, c) {
                acc += val * w;
                wsum += w;
            }
        }
        // Require at least half the interpolation weight to be backed by
        // real data; otherwise treat the point as nodata.
        if wsum >= 0.5 { Some(acc / wsum) } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surtgis_core::{CRS, GeoTransform};

    /// 4×4 lon/lat raster covering (0..4)°E × (0..4)°N, value = row*10 + col.
    fn lonlat_raster() -> Raster<f64> {
        let mut r = Raster::new(4, 4);
        for row in 0..4 {
            for col in 0..4 {
                r.set(row, col, (row * 10 + col) as f64).unwrap();
            }
        }
        r.set_transform(GeoTransform::new(0.0, 4.0, 1.0, -1.0));
        r.set_crs(Some(CRS::from_epsg(4326)));
        r
    }

    #[test]
    fn detects_crs_from_epsg() {
        let src = RasterSource::new(lonlat_raster(), None).unwrap();
        assert_eq!(src.crs(), SourceCrs::LonLat);
    }

    #[test]
    fn rejects_unsupported_epsg() {
        let mut r = lonlat_raster();
        r.set_crs(Some(CRS::from_epsg(32719))); // UTM 19S
        assert!(RasterSource::new(r, None).is_err());
    }

    #[test]
    fn heuristic_detects_degrees_without_crs() {
        let mut r = lonlat_raster();
        r.set_crs(None);
        let src = RasterSource::new(r, None).unwrap();
        assert_eq!(src.crs(), SourceCrs::LonLat);
    }

    #[test]
    fn nearest_sample_hits_cell_value() {
        let src = RasterSource::new(lonlat_raster(), None).unwrap();
        // Center of cell (row 0, col 2) is lon 2.5, lat 3.5.
        let (mx, my) = mercator::lonlat_to_meters(2.5, 3.5);
        assert_eq!(src.sample(mx, my, Resampling::Nearest), Some(2.0));
        // Center of cell (row 3, col 0) is lon 0.5, lat 0.5.
        let (mx, my) = mercator::lonlat_to_meters(0.5, 0.5);
        assert_eq!(src.sample(mx, my, Resampling::Nearest), Some(30.0));
    }

    #[test]
    fn bilinear_interpolates_between_centers() {
        let src = RasterSource::new(lonlat_raster(), None).unwrap();
        // Halfway between centers of (3,0)=30 and (3,1)=31: lon 1.0, lat 0.5.
        let (mx, my) = mercator::lonlat_to_meters(1.0, 0.5);
        let v = src.sample(mx, my, Resampling::Bilinear).unwrap();
        assert!((v - 30.5).abs() < 1e-9, "got {v}");
    }

    #[test]
    fn outside_raster_is_none() {
        let src = RasterSource::new(lonlat_raster(), None).unwrap();
        let (mx, my) = mercator::lonlat_to_meters(10.0, 10.0);
        assert_eq!(src.sample(mx, my, Resampling::Bilinear), None);
    }

    #[test]
    fn nodata_is_none_and_does_not_bleed() {
        let mut r = lonlat_raster();
        r.set_nodata(Some(-9999.0));
        r.set(0, 2, -9999.0).unwrap();
        let src = RasterSource::new(r, None).unwrap();
        let (mx, my) = mercator::lonlat_to_meters(2.5, 3.5);
        assert_eq!(src.sample(mx, my, Resampling::Nearest), None);
        // A bilinear sample next to the hole still returns data (renormalized).
        let (mx, my) = mercator::lonlat_to_meters(1.9, 3.5);
        assert!(src.sample(mx, my, Resampling::Bilinear).is_some());
    }

    #[test]
    fn area_average_over_whole_raster_is_mean() {
        let src = RasterSource::new(lonlat_raster(), None).unwrap();
        let (mx0, my0) = mercator::lonlat_to_meters(0.0, 0.0);
        let (mx1, my1) = mercator::lonlat_to_meters(4.0, 4.0);
        let v = src.sample_area(mx0, my0, mx1, my1).unwrap();
        // Mean of row*10+col over 4×4 = mean(rows)*10 + mean(cols) = 16.5.
        assert!((v - 16.5).abs() < 1e-9, "got {v}");
    }

    #[test]
    fn area_average_ignores_nodata() {
        let mut r = lonlat_raster();
        r.set_nodata(Some(-9999.0));
        for col in 0..4 {
            for row in 0..4 {
                if !(row == 0 && col == 0) {
                    r.set(row, col, -9999.0).unwrap();
                }
            }
        }
        let src = RasterSource::new(r, None).unwrap();
        let (mx0, my0) = mercator::lonlat_to_meters(0.0, 0.0);
        let (mx1, my1) = mercator::lonlat_to_meters(4.0, 4.0);
        // Only cell (0,0)=0.0 is valid.
        assert_eq!(src.sample_area(mx0, my0, mx1, my1), Some(0.0));
    }

    #[test]
    fn area_average_sub_cell_footprint_falls_back_to_point() {
        let src = RasterSource::new(lonlat_raster(), None).unwrap();
        // Tiny footprint centered on cell (3,0)'s center.
        let (mx, my) = mercator::lonlat_to_meters(0.5, 0.5);
        let v = src.sample_area(mx - 1.0, my - 1.0, mx + 1.0, my + 1.0).unwrap();
        assert!((v - 30.0).abs() < 1e-9, "got {v}");
    }

    #[test]
    fn native_zoom_is_sane_for_one_degree_cells() {
        let src = RasterSource::new(lonlat_raster(), None).unwrap();
        // 1° ≈ 111 km/pixel → around zoom 1.
        assert!(src.native_max_zoom(256) <= 2);
    }
}
