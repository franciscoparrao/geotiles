//! Web Mercator (EPSG:3857) tile math for the XYZ scheme.
//!
//! Conventions:
//! - Tile coordinates follow the XYZ ("slippy map") scheme: `(0, 0)` is the
//!   top-left (north-west) tile; `y` grows southwards.
//! - MBTiles uses the TMS scheme (`y` grows northwards); use
//!   [`TileCoord::tms_row`] when writing MBTiles.
//! - All linear units are meters in the spherical Web Mercator projection.

use std::f64::consts::PI;

/// WGS84 ellipsoid semi-major axis, used as the Web Mercator sphere radius.
pub const EARTH_RADIUS_M: f64 = 6_378_137.0;

/// Half the extent of the Web Mercator plane (≈ 20037508.34 m).
pub const ORIGIN_SHIFT_M: f64 = PI * EARTH_RADIUS_M;

/// Maximum latitude representable in Web Mercator (square world).
pub const MAX_LATITUDE_DEG: f64 = 85.051_128_779_806_59;

/// Convert lon/lat degrees (EPSG:4326) to Web Mercator meters.
///
/// Latitude is clamped to [`MAX_LATITUDE_DEG`] so poles map to the edge of
/// the Mercator square instead of infinity.
pub fn lonlat_to_meters(lon: f64, lat: f64) -> (f64, f64) {
    let lat = lat.clamp(-MAX_LATITUDE_DEG, MAX_LATITUDE_DEG);
    let mx = lon.to_radians() * EARTH_RADIUS_M;
    let my = ((PI / 4.0 + lat.to_radians() / 2.0).tan()).ln() * EARTH_RADIUS_M;
    (mx, my)
}

/// Convert Web Mercator meters to lon/lat degrees (EPSG:4326).
pub fn meters_to_lonlat(mx: f64, my: f64) -> (f64, f64) {
    let lon = (mx / EARTH_RADIUS_M).to_degrees();
    let lat = (2.0 * (my / EARTH_RADIUS_M).exp().atan() - PI / 2.0).to_degrees();
    (lon, lat)
}

/// Ground resolution in meters/pixel at the equator for a zoom level.
pub fn resolution(zoom: u8, tile_size: u32) -> f64 {
    2.0 * ORIGIN_SHIFT_M / (tile_size as f64 * (1u64 << zoom) as f64)
}

/// Smallest zoom level whose resolution is at least as fine as `res_m`.
///
/// This is the natural "max zoom" for a raster with native resolution
/// `res_m` meters/pixel: tiling deeper adds no information.
pub fn zoom_for_resolution(res_m: f64, tile_size: u32) -> u8 {
    debug_assert!(res_m > 0.0);
    for z in 0..=30u8 {
        if resolution(z, tile_size) <= res_m {
            return z;
        }
    }
    30
}

/// A tile address in the XYZ scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoord {
    /// Zoom level (0 = single world tile).
    pub z: u8,
    /// Column, west to east, in `0..2^z`.
    pub x: u32,
    /// Row, north to south, in `0..2^z` (XYZ convention).
    pub y: u32,
}

impl TileCoord {
    /// Number of tiles per axis at this zoom (`2^z`).
    pub fn tiles_per_axis(z: u8) -> u32 {
        1u32 << z
    }

    /// Bounds of this tile in Web Mercator meters: `(min_x, min_y, max_x, max_y)`.
    pub fn bounds_meters(&self) -> (f64, f64, f64, f64) {
        let span = 2.0 * ORIGIN_SHIFT_M / Self::tiles_per_axis(self.z) as f64;
        let min_x = -ORIGIN_SHIFT_M + self.x as f64 * span;
        let max_y = ORIGIN_SHIFT_M - self.y as f64 * span;
        (min_x, max_y - span, min_x + span, max_y)
    }

    /// Bounds of this tile in lon/lat degrees: `(west, south, east, north)`.
    pub fn bounds_lonlat(&self) -> (f64, f64, f64, f64) {
        let (min_x, min_y, max_x, max_y) = self.bounds_meters();
        let (west, south) = meters_to_lonlat(min_x, min_y);
        let (east, north) = meters_to_lonlat(max_x, max_y);
        (west, south, east, north)
    }

    /// Row index in the TMS scheme (used by MBTiles): `2^z - 1 - y`.
    pub fn tms_row(&self) -> u32 {
        Self::tiles_per_axis(self.z) - 1 - self.y
    }
}

/// Inclusive range of tiles covering a Web Mercator bbox at one zoom level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRange {
    pub z: u8,
    pub x_min: u32,
    pub x_max: u32,
    pub y_min: u32,
    pub y_max: u32,
}

impl TileRange {
    /// Tiles covering `(min_x, min_y, max_x, max_y)` meters at zoom `z`.
    ///
    /// The bbox is clamped to the Mercator square; max edges falling exactly
    /// on a tile boundary do not spill into the next tile.
    pub fn for_bounds(bounds_m: (f64, f64, f64, f64), z: u8) -> Self {
        let n = TileCoord::tiles_per_axis(z);
        let span = 2.0 * ORIGIN_SHIFT_M / n as f64;
        // Nudge max edges inward so exact boundaries stay in their tile.
        let eps = span * 1e-9;

        let col = |x: f64| ((x + ORIGIN_SHIFT_M) / span).floor();
        let row = |y: f64| ((ORIGIN_SHIFT_M - y) / span).floor();

        let clamp = |v: f64| (v.max(0.0) as u32).min(n - 1);

        Self {
            z,
            x_min: clamp(col(bounds_m.0)),
            x_max: clamp(col(bounds_m.2 - eps)),
            y_min: clamp(row(bounds_m.3)),
            y_max: clamp(row(bounds_m.1 + eps)),
        }
    }

    /// Number of tiles in the range.
    pub fn count(&self) -> u64 {
        (self.x_max - self.x_min + 1) as u64 * (self.y_max - self.y_min + 1) as u64
    }

    /// Iterate over all tile coordinates in the range, row-major.
    pub fn iter(&self) -> impl Iterator<Item = TileCoord> + use<> {
        let TileRange { z, x_min, x_max, y_min, y_max } = *self;
        (y_min..=y_max)
            .flat_map(move |y| (x_min..=x_max).map(move |x| TileCoord { z, x, y }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() < tol, "{a} != {b} (tol {tol})");
    }

    #[test]
    fn lonlat_meters_roundtrip() {
        for &(lon, lat) in &[(0.0, 0.0), (-71.6, -33.4), (179.9, 84.9), (-180.0, -85.0)] {
            let (mx, my) = lonlat_to_meters(lon, lat);
            let (lon2, lat2) = meters_to_lonlat(mx, my);
            assert_close(lon, lon2, 1e-9);
            assert_close(lat, lat2, 1e-9);
        }
    }

    #[test]
    fn known_projection_values() {
        let (mx, my) = lonlat_to_meters(180.0, 0.0);
        assert_close(mx, ORIGIN_SHIFT_M, 1e-6);
        assert_close(my, 0.0, 1e-6);
        let (_, my) = lonlat_to_meters(0.0, MAX_LATITUDE_DEG);
        assert_close(my, ORIGIN_SHIFT_M, 1e-4);
    }

    #[test]
    fn zoom_zero_tile_covers_world() {
        let t = TileCoord { z: 0, x: 0, y: 0 };
        let (min_x, min_y, max_x, max_y) = t.bounds_meters();
        assert_close(min_x, -ORIGIN_SHIFT_M, 1e-6);
        assert_close(min_y, -ORIGIN_SHIFT_M, 1e-6);
        assert_close(max_x, ORIGIN_SHIFT_M, 1e-6);
        assert_close(max_y, ORIGIN_SHIFT_M, 1e-6);
    }

    #[test]
    fn resolution_halves_per_zoom() {
        assert_close(resolution(0, 256), 156_543.033_928_041, 1e-6);
        assert_close(resolution(1, 256), resolution(0, 256) / 2.0, 1e-9);
        assert_eq!(zoom_for_resolution(156_543.04, 256), 0);
        assert_eq!(zoom_for_resolution(30.0, 256), 13);
    }

    #[test]
    fn tms_flip() {
        let t = TileCoord { z: 3, x: 1, y: 0 };
        assert_eq!(t.tms_row(), 7);
        let t = TileCoord { z: 3, x: 1, y: 7 };
        assert_eq!(t.tms_row(), 0);
    }

    #[test]
    fn tile_range_for_quadrant() {
        // NE quadrant of the world at z1 must be exactly tile (1, 0).
        let r = TileRange::for_bounds((0.0, 0.0, ORIGIN_SHIFT_M, ORIGIN_SHIFT_M), 1);
        assert_eq!((r.x_min, r.x_max, r.y_min, r.y_max), (1, 1, 0, 0));
        assert_eq!(r.count(), 1);
    }

    #[test]
    fn tile_range_clamps_to_world() {
        let huge = (-1e9, -1e9, 1e9, 1e9);
        let r = TileRange::for_bounds(huge, 2);
        assert_eq!((r.x_min, r.x_max, r.y_min, r.y_max), (0, 3, 0, 3));
        assert_eq!(r.count(), 16);
        assert_eq!(r.iter().count(), 16);
    }

    #[test]
    fn santiago_tile_z10() {
        // Santiago de Chile (-70.65, -33.45) is in tile (311, 613) at z10.
        let (mx, my) = lonlat_to_meters(-70.65, -33.45);
        let r = TileRange::for_bounds((mx, my, mx, my), 10);
        assert_eq!((r.x_min, r.y_min), (311, 613));
    }
}
