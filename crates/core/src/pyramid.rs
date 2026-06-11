//! Tile pyramid generation: render Web Mercator tiles from a raster source.

use surtgis_colormap::{ColorScheme, ColormapParams, auto_params, raster_to_rgba, rgba_to_png_bytes};
use surtgis_core::Raster;

use crate::error::{Error, Result};
use crate::mercator::{TileCoord, TileRange};
use crate::source::{RasterSource, Resampling};

/// Options controlling pyramid generation.
#[derive(Debug, Clone)]
pub struct PyramidOptions {
    /// Lowest zoom to generate (default 0; cheap, since tiles outside the
    /// source bounds are never rendered).
    pub min_zoom: Option<u8>,
    /// Highest zoom to generate (default: the source's native zoom).
    pub max_zoom: Option<u8>,
    /// Tile edge in pixels (default 256).
    pub tile_size: u32,
    /// Resampling used when reading the source.
    pub resampling: Resampling,
    /// Colour scheme applied to the band values.
    pub scheme: ColorScheme,
    /// Fixed (min, max) stretch; default: computed from the source data.
    pub range: Option<(f64, f64)>,
    /// Layer name recorded in the output metadata.
    pub name: String,
}

impl Default for PyramidOptions {
    fn default() -> Self {
        Self {
            min_zoom: None,
            max_zoom: None,
            tile_size: 256,
            resampling: Resampling::default(),
            scheme: ColorScheme::Grayscale,
            range: None,
            name: "geotiles".into(),
        }
    }
}

/// Metadata describing a finished pyramid, handed to the sink on finalize.
#[derive(Debug, Clone)]
pub struct PyramidMetadata {
    pub name: String,
    /// `(west, south, east, north)` in lon/lat degrees.
    pub bounds_lonlat: (f64, f64, f64, f64),
    pub min_zoom: u8,
    pub max_zoom: u8,
    /// Tile image format ("png").
    pub format: &'static str,
}

/// Counters returned by [`generate`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PyramidStats {
    /// Tiles actually written.
    pub written: u64,
    /// Tiles inside the bbox that turned out fully empty and were skipped.
    pub skipped: u64,
}

/// Destination for rendered tiles (XYZ tree, MBTiles, …).
pub trait TileSink {
    /// Store one encoded PNG tile.
    fn put(&mut self, coord: TileCoord, png: &[u8]) -> Result<()>;
    /// Called once after all tiles are written.
    fn finalize(&mut self, meta: &PyramidMetadata) -> Result<()>;
}

/// How sampled band values become RGBA pixels.
enum Shader {
    /// Single band through a colour scheme.
    Colormap(ColormapParams),
    /// 3 (RGB) or 4 (RGBA) bands stretched linearly onto 0..255.
    Rgb { lo: f64, inv_span: f64 },
}

/// Sample one band over the tile grid; `f(j, i, value)` receives each hit.
fn sample_grid<F: FnMut(usize, usize, f64)>(
    source: &RasterSource,
    band: usize,
    coord: TileCoord,
    tile_size: u32,
    resampling: Resampling,
    area_average: bool,
    mut f: F,
) {
    let (min_x, _, _, max_y) = coord.bounds_meters();
    let res = crate::mercator::resolution(coord.z, tile_size);
    let n = tile_size as usize;
    for i in 0..n {
        let my = max_y - (i as f64 + 0.5) * res;
        for j in 0..n {
            let mx = min_x + (j as f64 + 0.5) * res;
            let sampled = if area_average {
                source.sample_area_band(
                    band,
                    mx - res / 2.0,
                    my - res / 2.0,
                    mx + res / 2.0,
                    my + res / 2.0,
                )
            } else {
                source.sample_band(band, mx, my, resampling)
            };
            if let Some(v) = sampled {
                f(j, i, v);
            }
        }
    }
}

/// Render a 3/4-band source straight to an RGBA buffer.
///
/// Channel values are stretched linearly from `[lo, lo + 1/inv_span]` to
/// 0..255. Pixels where any colour band is missing become transparent; a
/// fourth band, when present, is used as the alpha channel.
fn render_tile_rgb(
    source: &RasterSource,
    coord: TileCoord,
    tile_size: u32,
    resampling: Resampling,
    area_average: bool,
    lo: f64,
    inv_span: f64,
) -> Option<Vec<u8>> {
    let n = tile_size as usize;
    let mut rgba = vec![0u8; n * n * 4];
    let mut hits = vec![0u8; n * n];

    for band in 0..3 {
        sample_grid(source, band, coord, tile_size, resampling, area_average, |j, i, v| {
            let t = ((v - lo) * inv_span * 255.0).clamp(0.0, 255.0);
            rgba[(i * n + j) * 4 + band] = t as u8;
            hits[i * n + j] += 1;
        });
    }
    // Opaque where all three colour bands resolved.
    let mut any = false;
    for (px, &h) in hits.iter().enumerate() {
        if h == 3 {
            rgba[px * 4 + 3] = 255;
            any = true;
        }
    }
    if !any {
        return None;
    }
    if source.band_count() == 4 {
        sample_grid(source, 3, coord, tile_size, resampling, area_average, |j, i, v| {
            let px = i * n + j;
            if hits[px] == 3 {
                let a = ((v - lo) * inv_span * 255.0).clamp(0.0, 255.0);
                rgba[px * 4 + 3] = a as u8;
            }
        });
    }
    Some(rgba)
}

/// Render one tile to an encoded PNG, or `None` when every pixel falls
/// outside the source or on nodata.
fn render_tile_png(
    source: &RasterSource,
    shader: &Shader,
    coord: TileCoord,
    tile_size: u32,
    resampling: Resampling,
) -> Result<Option<Vec<u8>>> {
    let res = crate::mercator::resolution(coord.z, tile_size);
    // At overview zooms one output pixel spans several source cells; point
    // sampling would skip most of the data (or all of it, for a raster
    // smaller than the pixel spacing). Switch to area averaging there.
    let area_average = res > 2.0 * source.native_resolution_m();

    let rgba = match shader {
        Shader::Colormap(params) => {
            let n = tile_size as usize;
            let mut tile = Raster::filled(n, n, f64::NAN);
            let mut any = false;
            sample_grid(source, 0, coord, tile_size, resampling, area_average, |j, i, v| {
                // Raster::set on a freshly allocated grid cannot fail in-bounds.
                let _ = tile.set(i, j, v);
                any = true;
            });
            if !any {
                return Ok(None);
            }
            raster_to_rgba(&tile, params)
        }
        Shader::Rgb { lo, inv_span } => {
            match render_tile_rgb(source, coord, tile_size, resampling, area_average, *lo, *inv_span)
            {
                Some(rgba) => rgba,
                None => return Ok(None),
            }
        }
    };
    rgba_to_png_bytes(tile_size, tile_size, &rgba)
        .map(Some)
        .map_err(|e| Error::Encode(e.to_string()))
}

/// Resolve the effective zoom range and shader for a run.
fn resolve(source: &RasterSource, opts: &PyramidOptions) -> Result<(u8, u8, Shader)> {
    let max_zoom = opts.max_zoom.unwrap_or_else(|| source.native_max_zoom(opts.tile_size));
    let min_zoom = opts.min_zoom.unwrap_or(0);
    if min_zoom > max_zoom {
        return Err(Error::InvalidInput(format!(
            "min zoom {min_zoom} exceeds max zoom {max_zoom}"
        )));
    }
    if let Some((lo, hi)) = opts.range
        && lo >= hi
    {
        return Err(Error::InvalidInput(format!("invalid stretch range: {lo}..{hi}")));
    }
    let shader = if source.band_count() >= 3 {
        // RGB(A): default stretch assumes byte imagery.
        let (lo, hi) = opts.range.unwrap_or((0.0, 255.0));
        Shader::Rgb { lo, inv_span: 1.0 / (hi - lo) }
    } else {
        Shader::Colormap(match opts.range {
            Some((lo, hi)) => ColormapParams::with_range(opts.scheme, lo, hi),
            None => auto_params(source.raster(), opts.scheme),
        })
    };
    Ok((min_zoom, max_zoom, shader))
}

/// Total tiles that would be rendered for the source at the given options.
///
/// Useful to size progress bars before calling [`generate`].
pub fn count_tiles(source: &RasterSource, opts: &PyramidOptions) -> Result<u64> {
    let (min_zoom, max_zoom, _) = resolve(source, opts)?;
    let bounds = source.bounds_meters();
    Ok((min_zoom..=max_zoom)
        .map(|z| TileRange::for_bounds(bounds, z).count())
        .sum())
}

/// Generate the full pyramid into `sink`.
///
/// Tiles are rendered in parallel (rayon) and handed to the sink from a
/// single writer thread, so sinks need no internal synchronization.
/// `progress` is invoked once per processed tile with the running count.
pub fn generate<S, F>(
    source: &RasterSource,
    opts: &PyramidOptions,
    sink: &mut S,
    progress: F,
) -> Result<PyramidStats>
where
    S: TileSink + Send,
    F: Fn(u64) + Sync,
{
    let (min_zoom, max_zoom, shader) = resolve(source, opts)?;
    let bounds = source.bounds_meters();

    let tiles: Vec<TileCoord> = (min_zoom..=max_zoom)
        .flat_map(|z| TileRange::for_bounds(bounds, z).iter())
        .collect();

    let stats = run_tiles(source, opts, &shader, &tiles, sink, &progress)?;

    sink.finalize(&PyramidMetadata {
        name: opts.name.clone(),
        bounds_lonlat: source.bounds_lonlat(),
        min_zoom,
        max_zoom,
        format: "png",
    })?;
    Ok(stats)
}

#[cfg(feature = "parallel")]
fn run_tiles<S, F>(
    source: &RasterSource,
    opts: &PyramidOptions,
    shader: &Shader,
    tiles: &[TileCoord],
    sink: &mut S,
    progress: &F,
) -> Result<PyramidStats>
where
    S: TileSink + Send,
    F: Fn(u64) + Sync,
{
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc;

    let done = AtomicU64::new(0);
    let (tx, rx) = mpsc::sync_channel::<(TileCoord, Vec<u8>)>(256);

    std::thread::scope(|scope| {
        let writer = scope.spawn(move || -> Result<u64> {
            let mut written = 0u64;
            for (coord, png) in rx {
                sink.put(coord, &png)?;
                written += 1;
            }
            Ok(written)
        });

        let render_result: Result<()> = tiles
            .par_iter()
            .try_for_each_init(
                || tx.clone(),
                |tx, &coord| {
                    if let Some(png) =
                        render_tile_png(source, shader, coord, opts.tile_size, opts.resampling)?
                    {
                        // The writer only hangs up on error; surfaced below.
                        let _ = tx.send((coord, png));
                    }
                    progress(done.fetch_add(1, Ordering::Relaxed) + 1);
                    Ok(())
                },
            );
        drop(tx);

        let written = writer.join().expect("tile writer thread panicked")?;
        render_result?;
        Ok(PyramidStats {
            written,
            skipped: tiles.len() as u64 - written,
        })
    })
}

#[cfg(not(feature = "parallel"))]
fn run_tiles<S, F>(
    source: &RasterSource,
    opts: &PyramidOptions,
    shader: &Shader,
    tiles: &[TileCoord],
    sink: &mut S,
    progress: &F,
) -> Result<PyramidStats>
where
    S: TileSink + Send,
    F: Fn(u64) + Sync,
{
    let mut stats = PyramidStats::default();
    for (i, &coord) in tiles.iter().enumerate() {
        match render_tile_png(source, shader, coord, opts.tile_size, opts.resampling)? {
            Some(png) => {
                sink.put(coord, &png)?;
                stats.written += 1;
            }
            None => stats.skipped += 1,
        }
        progress(i as u64 + 1);
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use surtgis_core::{CRS, GeoTransform};

    struct MemSink {
        tiles: HashMap<(u8, u32, u32), Vec<u8>>,
        meta: Option<PyramidMetadata>,
    }

    impl MemSink {
        fn new() -> Self {
            Self { tiles: HashMap::new(), meta: None }
        }
    }

    impl TileSink for MemSink {
        fn put(&mut self, c: TileCoord, png: &[u8]) -> Result<()> {
            self.tiles.insert((c.z, c.x, c.y), png.to_vec());
            Ok(())
        }
        fn finalize(&mut self, meta: &PyramidMetadata) -> Result<()> {
            self.meta = Some(meta.clone());
            Ok(())
        }
    }

    /// Gradient raster over (-5..5)°lon × (-5..5)°lat.
    fn source() -> RasterSource {
        let n = 64;
        let mut r = Raster::new(n, n);
        for i in 0..n {
            for j in 0..n {
                r.set(i, j, (i + j) as f64).unwrap();
            }
        }
        r.set_transform(GeoTransform::new(-5.0, 5.0, 10.0 / n as f64, -10.0 / n as f64));
        r.set_crs(Some(CRS::from_epsg(4326)));
        RasterSource::new(r, None).unwrap()
    }

    #[test]
    fn generates_pyramid_with_valid_pngs() {
        let src = source();
        let opts = PyramidOptions {
            min_zoom: Some(0),
            max_zoom: Some(4),
            ..Default::default()
        };
        let mut sink = MemSink::new();
        let stats = generate(&src, &opts, &mut sink, |_| {}).unwrap();

        assert!(stats.written >= 5, "at least one tile per zoom level");
        assert_eq!(sink.tiles.len() as u64, stats.written);
        // Every blob is a PNG (magic bytes).
        for png in sink.tiles.values() {
            assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n']);
        }
        // z0 world tile must exist; source straddles the equator/meridian.
        assert!(sink.tiles.contains_key(&(0, 0, 0)));
        let meta = sink.meta.unwrap();
        assert_eq!((meta.min_zoom, meta.max_zoom), (0, 4));
        assert!((meta.bounds_lonlat.0 - -5.0).abs() < 1e-9);
    }

    #[test]
    fn count_matches_enumeration() {
        let src = source();
        let opts = PyramidOptions {
            min_zoom: Some(0),
            max_zoom: Some(3),
            ..Default::default()
        };
        let counted = count_tiles(&src, &opts).unwrap();
        let mut sink = MemSink::new();
        let stats = generate(&src, &opts, &mut sink, |_| {}).unwrap();
        assert_eq!(counted, stats.written + stats.skipped);
    }

    #[test]
    fn rgb_source_renders_color_tiles() {
        // Constant-colour RGB bands: r=200, g=100, b=50.
        let n = 64;
        let mut bands = vec![];
        for value in [200.0, 100.0, 50.0] {
            let mut r = Raster::filled(n, n, value);
            r.set_transform(GeoTransform::new(-5.0, 5.0, 10.0 / n as f64, -10.0 / n as f64));
            r.set_crs(Some(CRS::from_epsg(4326)));
            bands.push(r);
        }
        let src = RasterSource::new_multi(bands, None).unwrap();
        assert_eq!(src.band_count(), 3);

        let opts = PyramidOptions {
            min_zoom: Some(2),
            max_zoom: Some(4),
            ..Default::default()
        };
        let mut sink = MemSink::new();
        let stats = generate(&src, &opts, &mut sink, |_| {}).unwrap();
        assert!(stats.written > 0);
        for png in sink.tiles.values() {
            assert_eq!(&png[..4], &[0x89, b'P', b'N', b'G']);
        }
    }

    #[test]
    fn rgba_alpha_band_controls_transparency() {
        let n = 64;
        let mut bands = vec![];
        for value in [200.0, 100.0, 50.0, 0.0] {
            let mut r = Raster::filled(n, n, value);
            r.set_transform(GeoTransform::new(-5.0, 5.0, 10.0 / n as f64, -10.0 / n as f64));
            r.set_crs(Some(CRS::from_epsg(4326)));
            bands.push(r);
        }
        let src = RasterSource::new_multi(bands, None).unwrap();
        // Alpha band of zeros must still produce (fully transparent) tiles,
        // exercising the 4-band path end to end.
        let opts = PyramidOptions {
            min_zoom: Some(3),
            max_zoom: Some(3),
            ..Default::default()
        };
        let mut sink = MemSink::new();
        let stats = generate(&src, &opts, &mut sink, |_| {}).unwrap();
        assert!(stats.written > 0);
    }

    #[test]
    fn small_raster_covers_every_low_zoom() {
        // A raster much smaller than a z0..z4 pixel: with point sampling it
        // fell between pixel centers and produced empty low-zoom tiles.
        let n = 32;
        let mut r = Raster::new(n, n);
        for i in 0..n {
            for j in 0..n {
                r.set(i, j, (i + j) as f64).unwrap();
            }
        }
        // ~0.06° wide near Valparaíso.
        r.set_transform(GeoTransform::new(-71.5, -32.8, 0.002, -0.002));
        r.set_crs(Some(CRS::from_epsg(4326)));
        let src = RasterSource::new(r, None).unwrap();

        let opts = PyramidOptions {
            min_zoom: Some(0),
            max_zoom: Some(6),
            ..Default::default()
        };
        let mut sink = MemSink::new();
        generate(&src, &opts, &mut sink, |_| {}).unwrap();
        for z in 0..=6u8 {
            assert!(
                sink.tiles.keys().any(|&(tz, _, _)| tz == z),
                "zoom {z} has no tiles"
            );
        }
    }

    #[test]
    fn rejects_inverted_zooms() {
        let src = source();
        let opts = PyramidOptions {
            min_zoom: Some(5),
            max_zoom: Some(2),
            ..Default::default()
        };
        let mut sink = MemSink::new();
        assert!(generate(&src, &opts, &mut sink, |_| {}).is_err());
    }
}
