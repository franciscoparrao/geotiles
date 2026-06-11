//! Raster source abstraction: CRS handling and resampling.
//!
//! v0.1 supports sources in EPSG:4326 (lon/lat) and EPSG:3857 (Web Mercator).
//! Reprojection between those and the tile grid is analytic, so no external
//! projection engine is needed.

use surtgis_core::Raster;

use crate::error::{Error, Result};
use crate::mercator::{self, MAX_LATITUDE_DEG, ORIGIN_SHIFT_M};

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

/// A tileable raster: data plus the analytic projection to Web Mercator.
#[derive(Debug)]
pub struct RasterSource {
    raster: Raster<f64>,
    crs: SourceCrs,
}

impl RasterSource {
    /// Wrap a raster, detecting its CRS.
    ///
    /// Detection order: explicit `crs_override`, then the raster's EPSG code
    /// (4326 → lon/lat, 3857/900913 → mercator), then a bounds heuristic
    /// (coordinates within ±180/±90 look like degrees). Anything else is
    /// rejected — reproject the input first.
    pub fn new(raster: Raster<f64>, crs_override: Option<SourceCrs>) -> Result<Self> {
        if raster.rows() == 0 || raster.cols() == 0 {
            return Err(Error::InvalidInput("empty raster".into()));
        }
        if !raster.transform().is_north_up() {
            return Err(Error::InvalidInput(
                "rotated rasters are not supported; warp to north-up first".into(),
            ));
        }

        let crs = match crs_override {
            Some(crs) => crs,
            None => Self::detect_crs(&raster)?,
        };
        Ok(Self { raster, crs })
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

    /// Borrow the underlying raster.
    pub fn raster(&self) -> &Raster<f64> {
        &self.raster
    }

    /// Source bounds in Web Mercator meters `(min_x, min_y, max_x, max_y)`.
    pub fn bounds_meters(&self) -> (f64, f64, f64, f64) {
        let (min_x, min_y, max_x, max_y) = self.raster.bounds();
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
        let (min_x, min_y, max_x, max_y) = self.raster.bounds();
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
        let cell = self.raster.cell_size();
        match self.crs {
            SourceCrs::Mercator => cell,
            SourceCrs::LonLat => cell * ORIGIN_SHIFT_M / 180.0,
        }
    }

    /// Natural maximum zoom: tiling deeper than this adds no detail.
    pub fn native_max_zoom(&self, tile_size: u32) -> u8 {
        mercator::zoom_for_resolution(self.native_resolution_m(), tile_size)
    }

    /// Sample the source at a Web Mercator point. `None` means outside the
    /// raster or nodata.
    pub fn sample(&self, mx: f64, my: f64, method: Resampling) -> Option<f64> {
        let (sx, sy) = match self.crs {
            SourceCrs::Mercator => (mx, my),
            SourceCrs::LonLat => mercator::meters_to_lonlat(mx, my),
        };
        let (col, row) = self.raster.geo_to_pixel(sx, sy);
        if !col.is_finite() || !row.is_finite() {
            return None;
        }
        match method {
            Resampling::Nearest => self.sample_nearest(col, row),
            Resampling::Bilinear => self.sample_bilinear(col, row),
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
        let ((sx0, sy0), (sx1, sy1)) = match self.crs {
            SourceCrs::Mercator => ((mx0, my0), (mx1, my1)),
            SourceCrs::LonLat => (
                mercator::meters_to_lonlat(mx0, my0),
                mercator::meters_to_lonlat(mx1, my1),
            ),
        };
        let (ca, ra) = self.raster.geo_to_pixel(sx0.min(sx1), sy0.max(sy1));
        let (cb, rb) = self.raster.geo_to_pixel(sx0.max(sx1), sy0.min(sy1));
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
            return self.sample((mx0 + mx1) / 2.0, (my0 + my1) / 2.0, Resampling::Bilinear);
        }

        // Intersect with the raster so footprints that poke outside don't
        // iterate over (possibly millions of) nonexistent cells.
        let col_start = col_start.max(0);
        let col_end = col_end.min(self.raster.cols() as i64 - 1);
        let row_start = row_start.max(0);
        let row_end = row_end.min(self.raster.rows() as i64 - 1);

        let mut acc = 0.0;
        let mut count = 0u64;
        for row in row_start..=row_end {
            for col in col_start..=col_end {
                if let Some(v) = self.valid_at(row, col) {
                    acc += v;
                    count += 1;
                }
            }
        }
        (count > 0).then(|| acc / count as f64)
    }

    fn valid_at(&self, row: i64, col: i64) -> Option<f64> {
        if row < 0 || col < 0 || row >= self.raster.rows() as i64 || col >= self.raster.cols() as i64
        {
            return None;
        }
        let v = self.raster.get(row as usize, col as usize).ok()?;
        if v.is_nan() || self.raster.is_nodata(v) {
            None
        } else {
            Some(v)
        }
    }

    fn sample_nearest(&self, col: f64, row: f64) -> Option<f64> {
        self.valid_at(row.floor() as i64, col.floor() as i64)
    }

    /// Bilinear interpolation between pixel centers, renormalizing weights
    /// over the valid neighbours so nodata does not bleed into the result.
    fn sample_bilinear(&self, col: f64, row: f64) -> Option<f64> {
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
            if let Some(val) = self.valid_at(r, c) {
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
