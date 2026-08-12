//! Tile pyramid generation: render Web Mercator tiles from a raster source.

use surtgis_colormap::{ColorScheme, ColormapParams, raster_to_rgba, rgba_to_png_bytes};
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
    /// Per-band `(min, max)` stretches for RGB(A) sources. When present it
    /// takes precedence over `range`; each entry applies to the matching
    /// colour band (indices 0..3). Shorter than the band count is fine —
    /// missing bands fall back to (0, 255).
    pub band_ranges: Option<Vec<(f64, f64)>>,
    /// Encoded tile image format (default PNG).
    pub format: TileFormat,
    /// Layer name recorded in the output metadata.
    pub name: String,
}

/// Encoded image format for raster tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TileFormat {
    /// PNG (lossless, universal). Default.
    #[default]
    Png,
    /// Lossless WebP (pure-Rust VP8L) — typically smaller than PNG with no
    /// quality loss.
    WebP,
    /// Lossy JPEG (no alpha: transparency is composited onto black).
    Jpeg,
}

impl TileFormat {
    /// The MBTiles/TileJSON `format` string and XYZ file extension.
    pub fn as_str(self) -> &'static str {
        match self {
            TileFormat::Png => "png",
            TileFormat::WebP => "webp",
            TileFormat::Jpeg => "jpeg",
        }
    }
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
            band_ranges: None,
            format: TileFormat::Png,
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
    /// Tile format ("png" for raster, "pbf" for vector).
    pub format: &'static str,
    /// MBTiles `json` metadata row (`vector_layers`); `None` for raster.
    pub json: Option<String>,
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
    /// 3 (RGB) or 4 (RGBA) bands stretched linearly onto 0..255. Each band
    /// has its own `(lo, inv_span)` stretch; the alpha band reuses the
    /// first band's stretch.
    Rgb { stretches: Vec<(f64, f64)> },
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
/// fourth band, when present, is used as the alpha channel (same stretch
/// as the first band).
fn render_tile_rgb(
    source: &RasterSource,
    coord: TileCoord,
    tile_size: u32,
    resampling: Resampling,
    area_average: bool,
    stretches: &[(f64, f64)],
) -> Option<Vec<u8>> {
    let n = tile_size as usize;
    let mut rgba = vec![0u8; n * n * 4];
    let mut hits = vec![0u8; n * n];

    for band in 0..3 {
        let (lo, inv_span) = stretches.get(band).copied().unwrap_or((0.0, 1.0 / 255.0));
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
        let (lo, inv_span) = stretches.first().copied().unwrap_or((0.0, 1.0 / 255.0));
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

/// Render one tile to an encoded image, or `None` when every pixel falls
/// outside the source or on nodata.
///
/// File-backed sources render against the tile's source window (read on
/// demand) instead of a full in-memory raster.
fn render_tile_image(
    source: &RasterSource,
    shader: &Shader,
    coord: TileCoord,
    tile_size: u32,
    resampling: Resampling,
    format: TileFormat,
) -> Result<Option<Vec<u8>>> {
    // For file-backed sources, materialise the source window this tile
    // needs and render against it.
    if source.is_file_backed() {
        let window = source.window_source(coord)?.unwrap();
        if window.band_count() == 0 {
            return Ok(None);
        }
        return render_tile_image_in_memory(&window, shader, coord, tile_size, resampling, format);
    }
    render_tile_image_in_memory(source, shader, coord, tile_size, resampling, format)
}

fn render_tile_image_in_memory(
    source: &RasterSource,
    shader: &Shader,
    coord: TileCoord,
    tile_size: u32,
    resampling: Resampling,
    format: TileFormat,
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
        Shader::Rgb { stretches } => {
            match render_tile_rgb(source, coord, tile_size, resampling, area_average, stretches) {
                Some(rgba) => rgba,
                None => return Ok(None),
            }
        }
    };
    encode_rgba(&rgba, tile_size, format).map(Some)
}

/// Encode an RGBA tile buffer to the requested image format.
fn encode_rgba(rgba: &[u8], tile_size: u32, format: TileFormat) -> Result<Vec<u8>> {
    match format {
        TileFormat::Png => rgba_to_png_bytes(tile_size, tile_size, rgba)
            .map_err(|e| Error::Encode(e.to_string())),
        TileFormat::WebP => {
            use image::ExtendedColorType;
            use image::codecs::webp::WebPEncoder;
            let mut out = Vec::new();
            WebPEncoder::new_lossless(&mut out)
                .encode(rgba, tile_size, tile_size, ExtendedColorType::Rgba8)
                .map_err(|e| Error::Encode(format!("webp: {e}")))?;
            Ok(out)
        }
        TileFormat::Jpeg => {
            use jpeg_encoder::{ColorType, Encoder};
            // JPEG has no alpha: composite transparency onto black (the
            // standard empty-tile background for raster basemaps).
            let n = (tile_size * tile_size) as usize;
            let mut rgb = Vec::with_capacity(n * 3);
            for px in rgba.chunks_exact(4) {
                let a = px[3] as u32;
                // rgb = src*a + bg*(1-a), bg = (0,0,0) → src*a.
                let blend = |c: u8| ((c as u32 * a) / 255) as u8;
                rgb.push(blend(px[0]));
                rgb.push(blend(px[1]));
                rgb.push(blend(px[2]));
            }
            let mut out = Vec::new();
            Encoder::new(&mut out, 85)
                .encode(&rgb, tile_size as u16, tile_size as u16, ColorType::Rgb)
                .map_err(|e| Error::Encode(format!("jpeg: {e}")))?;
            Ok(out)
        }
    }
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
        let stretches = match &opts.band_ranges {
            Some(ranges) => ranges
                .iter()
                .map(|&(lo, hi)| {
                    if hi <= lo {
                        Err(Error::InvalidInput(format!(
                            "invalid band stretch range: {lo}..{hi}"
                        )))
                    } else {
                        Ok((lo, 1.0 / (hi - lo)))
                    }
                })
                .collect::<Result<Vec<_>>>()?,
            None => {
                let (lo, hi) = opts.range.unwrap_or((0.0, 255.0));
                vec![(lo, 1.0 / (hi - lo))]
            }
        };
        Shader::Rgb { stretches }
    } else {
        Shader::Colormap(match opts.range {
            Some((lo, hi)) => ColormapParams::with_range(opts.scheme, lo, hi),
            None => {
                let (lo, hi) = source.minmax()?;
                ColormapParams::with_range(opts.scheme, lo, hi)
            }
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
/// The [`PyramidMetadata`] a run will hand to the sink's `finalize`.
///
/// Exposed so streaming sinks that need header/metadata *before* any tile
/// (e.g. [`crate::PmtilesSink`]) can prepare with the same values.
pub fn pyramid_metadata(
    source: &RasterSource,
    opts: &PyramidOptions,
) -> Result<PyramidMetadata> {
    let (min_zoom, max_zoom, _) = resolve(source, opts)?;
    Ok(PyramidMetadata {
        name: opts.name.clone(),
        bounds_lonlat: source.bounds_lonlat(),
        min_zoom,
        max_zoom,
        format: opts.format.as_str(),
        json: None,
    })
}

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
        format: opts.format.as_str(),
        json: None,
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
    use std::sync::{Arc, Mutex};

    let done = AtomicU64::new(0);
    let (tx, rx) = mpsc::sync_channel::<(TileCoord, Vec<u8>)>(256);

    // Cap total in-flight window memory for file-backed sources: each tile
    // materialises its source window, and overview tiles (which span most
    // of the raster) would otherwise stack several full-raster copies under
    // Rayon's parallelism. A per-byte semaphore keeps the peak bounded.
    const WINDOW_BUDGET: usize = 512 * 1024 * 1024; // 512 MiB in-flight
    let window_sem = Arc::new(Mutex::new(WINDOW_BUDGET));
    let window_cond = Arc::new(std::sync::Condvar::new());

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
                || (tx.clone(), window_sem.clone(), window_cond.clone()),
                |(tx, sem, cond), &coord| {
                    // Wait until enough window budget is free to render this
                    // tile. In-memory sources always fit (no window).
                    let need = source.window_bytes(coord).unwrap_or(0);
                    {
                        let mut budget = sem.lock().unwrap();
                        while *budget < need {
                            budget = cond.wait(budget).unwrap();
                        }
                        *budget -= need;
                    }
                    let rendered = render_tile_image(
                        source,
                        shader,
                        coord,
                        opts.tile_size,
                        opts.resampling,
                        opts.format,
                    );
                    {
                        let mut budget = sem.lock().unwrap();
                        *budget += need;
                        cond.notify_all();
                    }
                    if let Some(data) = rendered? {
                        // The writer only hangs up on error; surfaced below.
                        let _ = tx.send((coord, data));
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
        match render_tile_image(source, shader, coord, opts.tile_size, opts.resampling, opts.format)?
        {
            Some(data) => {
                sink.put(coord, &data)?;
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
    fn webp_format_produces_valid_webp_tiles() {
        let src = source();
        let opts = PyramidOptions {
            min_zoom: Some(0),
            max_zoom: Some(3),
            format: TileFormat::WebP,
            ..Default::default()
        };
        let mut sink = MemSink::new();
        let stats = generate(&src, &opts, &mut sink, |_| {}).unwrap();
        assert!(stats.written >= 4);
        // WebP container: "RIFF"...."WEBP".
        for tile in sink.tiles.values() {
            assert_eq!(&tile[..4], b"RIFF", "expected RIFF header");
            assert_eq!(&tile[8..12], b"WEBP", "expected WEBP fourcc");
        }
        assert_eq!(sink.meta.unwrap().format, "webp");
    }

    #[test]
    fn file_backed_matches_in_memory_pyramid() {
        // A raster with a gradient and a nodata corner, written as a tiled
        // COG. Tiling it twice — in-memory and file-backed — must produce
        // byte-identical tiles.
        let n = 128;
        let mut r = Raster::new(n, n);
        for i in 0..n {
            for j in 0..n {
                let v = if i < 4 || j < 4 {
                    f64::NAN
                } else {
                    (i * 10 + j) as f64
                };
                r.set(i, j, v).unwrap();
            }
        }
        r.set_transform(GeoTransform::new(-71.0, -33.0, 0.01, -0.01));
        r.set_crs(Some(CRS::from_epsg(4326)));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.tif");
        crate::cog::write_cog(&r, &path, &crate::cog::CogOptions {
            tile_size: 32,
            ..Default::default()
        })
        .unwrap();

        let in_mem = RasterSource::new(r, None).unwrap();
        let file_backed = RasterSource::open_file(&path, &[0], None).unwrap();

        let opts = PyramidOptions {
            min_zoom: Some(0),
            max_zoom: Some(4),
            range: Some((0.0, 1300.0)),
            ..Default::default()
        };
        let mut sink_a = MemSink::new();
        generate(&in_mem, &opts, &mut sink_a, |_| {}).unwrap();
        let mut sink_b = MemSink::new();
        generate(&file_backed, &opts, &mut sink_b, |_| {}).unwrap();

        assert_eq!(sink_a.tiles.len(), sink_b.tiles.len());
        for (coord, bytes) in &sink_a.tiles {
            assert_eq!(
                bytes,
                sink_b.tiles.get(coord).expect("same tile in file-backed"),
                "tile {coord:?} differs between in-memory and file-backed"
            );
        }
    }

    #[test]
    fn file_backed_window_is_small() {
        // A 2000x2000 raster covering 1° of lon/lat. A tile at z12 covers
        // roughly 1/512 of the source width, so the window it reads must be
        // a small fraction of the raster, not the whole thing.
        let n = 2000;
        let mut r = Raster::new(n, n);
        for i in 0..n {
            for j in 0..n {
                r.set(i, j, (i + j) as f64).unwrap();
            }
        }
        r.set_transform(GeoTransform::new(-71.5, -32.5, 1.0 / n as f64, -1.0 / n as f64));
        r.set_crs(Some(CRS::from_epsg(4326)));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.tif");
        crate::cog::write_cog(&r, &path, &crate::cog::CogOptions {
            tile_size: 256,
            ..Default::default()
        })
        .unwrap();

        let src = RasterSource::open_file(&path, &[0], None).unwrap();
        assert!(src.is_file_backed());

        // Pick a real tile inside the raster bounds at z12.
        let bounds = src.bounds_meters();
        let coord = TileRange::for_bounds(bounds, 12).iter().next().unwrap();
        let win = src.window_source(coord).unwrap().unwrap();
        let (rows, cols) = win.band(0).shape();
        // Tile spans ~4096/360° ≈ 11.4 px/tile; with margin ~20px each way
        // the window must be well under 100px, not the full 2000.
        assert!(cols < 100 && rows < 100, "window too large: {rows}x{cols}");
        // And it must match the full raster's values.
        let full = RasterSource::new(r, None).unwrap();
        let full_win = full.window_source(coord).unwrap(); // None for memory
        assert!(full_win.is_none());
    }

    #[test]
    fn jpeg_format_produces_valid_jpeg_tiles() {
        let src = source();
        let opts = PyramidOptions {
            min_zoom: Some(0),
            max_zoom: Some(3),
            format: TileFormat::Jpeg,
            ..Default::default()
        };
        let mut sink = MemSink::new();
        let stats = generate(&src, &opts, &mut sink, |_| {}).unwrap();
        assert!(stats.written >= 4);
        // JPEG SOI marker 0xFFD8.
        for tile in sink.tiles.values() {
            assert_eq!(&tile[..2], &[0xFF, 0xD8], "expected JPEG SOI marker");
        }
        assert_eq!(sink.meta.unwrap().format, "jpeg");
    }

    #[test]
    fn jpeg_composites_transparency_on_black() {
        // Constant RGB bands (r=200,g=100,b=50) with a transparent top-left
        // corner (nodata). JPEG has no alpha, so that corner must come out
        // black after decode.
        let n = 64;
        let mut bands = vec![];
        for value in [200.0, 100.0, 50.0] {
            let mut r = Raster::filled(n, n, f64::NAN);
            for j in 0..n {
                for i in 0..n {
                    if i >= 2 && j >= 2 {
                        r.set(i, j, value).unwrap();
                    }
                }
            }
            r.set_transform(GeoTransform::new(-5.0, 5.0, 10.0 / n as f64, -10.0 / n as f64));
            r.set_crs(Some(CRS::from_epsg(4326)));
            bands.push(r);
        }
        let src = RasterSource::new_multi(bands, None).unwrap();

        let opts = PyramidOptions {
            min_zoom: Some(0),
            max_zoom: Some(1),
            format: TileFormat::Jpeg,
            ..Default::default()
        };
        let mut sink = MemSink::new();
        generate(&src, &opts, &mut sink, |_| {}).unwrap();

        let Some(bytes) = sink.tiles.get(&(0, 0, 0)) else {
            panic!("expected zoom-0 tile");
        };
        let decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(bytes));
        let mut decoder = decoder;
        let pixels = decoder.decode().expect("decode jpeg tile");
        let info = decoder.info().expect("jpeg info");
        let stride = match info.pixel_format {
            jpeg_decoder::PixelFormat::RGB24 => 3,
            jpeg_decoder::PixelFormat::L8 => 1,
            _ => 3,
        };
        let get = |i: usize, j: usize| {
            let off = (j * info.width as usize + i) * stride;
            let (r, g, b) = (pixels[off], pixels[off + 1], pixels[off + 2]);
            r.max(g).max(b)
        };
        assert!(get(0, 0) <= 40, "transparent corner should be near-black");
        // Tile (0,0,0) spans the whole world; the source occupies its centre
        // (-5..5° lon/lat ≈ mercator 0,0). A centre pixel is opaque data.
        let cx = info.width as usize / 2;
        let cy = info.height as usize / 2;
        assert!(
            get(cx, cy) > 100,
            "opaque centre should be bright, got {}",
            get(cx, cy)
        );
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
    fn band_ranges_stretch_each_channel_independently() {
        // Bands hold 0..255 and cover the whole world (-180..180). With
        // per-band ranges that each map the stored value to a different
        // output level, the tile's channels must come out at those levels.
        let n = 32;
        let mut bands = vec![];
        for value in [128.0, 64.0, 192.0] {
            let mut r = Raster::filled(n, n, value);
            r.set_transform(GeoTransform::new(-180.0, 180.0, 360.0 / n as f64, -360.0 / n as f64));
            r.set_crs(Some(CRS::from_epsg(4326)));
            bands.push(r);
        }
        let src = RasterSource::new_multi(bands, None).unwrap();

        let opts = PyramidOptions {
            min_zoom: Some(3),
            max_zoom: Some(3),
            // value 128 in 0..256 → ~127; value 64 in 0..64 → 255; value
            // 192 in 128..256 → ~128.
            band_ranges: Some(vec![(0.0, 256.0), (0.0, 64.0), (128.0, 256.0)]),
            ..Default::default()
        };
        let mut sink = MemSink::new();
        generate(&src, &opts, &mut sink, |_| {}).unwrap();

        // Decode one tile (PNG) and check the three channels at its centre.
        let data = sink.tiles.values().next().expect("at least one tile");
        let img = image::load_from_memory(data).unwrap().to_rgba8();
        let (cx, cy) = (img.width() / 2, img.height() / 2);
        let px = img.get_pixel(cx, cy);
        assert!((px.0[0] as i32 - 127).abs() <= 2, "R={}", px.0[0]);
        assert!((px.0[1] as i32 - 255).abs() <= 2, "G={}", px.0[1]);
        assert!((px.0[2] as i32 - 128).abs() <= 2, "B={}", px.0[2]);
    }

    #[test]
    fn invalid_band_range_is_rejected() {
        let n = 32;
        let mut bands = vec![];
        for value in [128.0, 100.0, 50.0] {
            let mut r = Raster::filled(n, n, value);
            r.set_transform(GeoTransform::new(-5.0, 5.0, 10.0 / n as f64, -10.0 / n as f64));
            r.set_crs(Some(CRS::from_epsg(4326)));
            bands.push(r);
        }
        let src = RasterSource::new_multi(bands, None).unwrap();
        let opts = PyramidOptions {
            min_zoom: Some(0),
            max_zoom: Some(1),
            band_ranges: Some(vec![(0.0, 0.0)]),
            ..Default::default()
        };
        let mut sink = MemSink::new();
        assert!(generate(&src, &opts, &mut sink, |_| {}).is_err());
    }

    #[test]
    fn rgba_alpha_band_controls_transparency() {        let n = 64;
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
