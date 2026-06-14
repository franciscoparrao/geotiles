//! Mapbox Vector Tile (MVT) pyramid generation.
//!
//! Per tile: clip features to the buffered tile rect (`geo` BooleanOps),
//! simplify proportionally to the zoom resolution (Douglas-Peucker),
//! quantize to integer tile coordinates with winding normalization and
//! degenerate filtering, encode with the `mvt` crate, optionally gzip.

use std::io::Write as _;

use flate2::Compression as Flate;
use flate2::write::GzEncoder;
use geo::{BooleanOps, Simplify};
use geo_types::{Geometry, LineString, MultiLineString, MultiPolygon, Polygon, Rect, polygon};
use mvt::{GeomEncoder, GeomType, Tile as MvtTile};
use surtgis_core::vector::AttributeValue;

use crate::error::{Error, Result};
use crate::mercator::{TileCoord, TileRange};
use crate::pyramid::{PyramidMetadata, PyramidStats, TileSink};
use crate::vector::{VectorFeature, VectorSource};

/// Options for [`generate_mvt`].
#[derive(Debug, Clone)]
pub struct MvtOptions {
    /// Lowest zoom (default 0).
    pub min_zoom: u8,
    /// Highest zoom (default 14, tippecanoe's default).
    pub max_zoom: u8,
    /// Tile coordinate extent (default 4096).
    pub extent: u32,
    /// Clip buffer around the tile, in extent units (default 64).
    pub buffer: u32,
    /// Simplification tolerance in extent units (default 1.0; 0 disables).
    pub simplify: f64,
    /// gzip-compress encoded tiles (MBTiles convention). Disable for XYZ
    /// trees served by plain static file servers.
    pub compress: bool,
    /// Tileset name for the output metadata.
    pub name: String,
}

impl Default for MvtOptions {
    fn default() -> Self {
        Self {
            min_zoom: 0,
            max_zoom: 14,
            extent: 4096,
            buffer: 64,
            simplify: 1.0,
            compress: true,
            name: "geotiles".into(),
        }
    }
}

/// Generate a vector tile pyramid into `sink`.
///
/// Mirrors the raster [`generate`](crate::pyramid::generate): tiles render
/// in parallel, one writer thread feeds the sink, `progress` gets the
/// running count of processed tiles.
pub fn generate_mvt<S, F>(
    source: &VectorSource,
    opts: &MvtOptions,
    sink: &mut S,
    progress: F,
) -> Result<PyramidStats>
where
    S: TileSink + Send,
    F: Fn(u64) + Sync,
{
    if opts.min_zoom > opts.max_zoom {
        return Err(Error::InvalidInput(format!(
            "min zoom {} exceeds max zoom {}",
            opts.min_zoom, opts.max_zoom
        )));
    }
    if opts.extent == 0 {
        return Err(Error::InvalidInput("extent must be positive".into()));
    }

    let bounds = source.bounds_meters();
    let tiles: Vec<TileCoord> = (opts.min_zoom..=opts.max_zoom)
        .flat_map(|z| TileRange::for_bounds(bounds, z).iter())
        .collect();

    let stats = run_tiles(source, opts, &tiles, sink, &progress)?;

    sink.finalize(&PyramidMetadata {
        name: opts.name.clone(),
        bounds_lonlat: source.bounds_lonlat(),
        min_zoom: opts.min_zoom,
        max_zoom: opts.max_zoom,
        format: "pbf",
        json: Some(vector_layers_json(source, opts)),
    })?;
    Ok(stats)
}

/// MBTiles 1.3 requires a `json` metadata row describing vector layers.
fn vector_layers_json(source: &VectorSource, opts: &MvtOptions) -> String {
    let layers: Vec<String> = source
        .layers()
        .iter()
        .map(|layer| {
            // Field name → MVT type label, from the first value seen.
            let mut fields: Vec<(String, &'static str)> = vec![];
            for f in &layer.features {
                for (k, v) in &f.properties {
                    if fields.iter().any(|(fk, _)| fk == k) {
                        continue;
                    }
                    let t = match v {
                        AttributeValue::Bool(_) => "Boolean",
                        AttributeValue::Int(_) | AttributeValue::Float(_) => "Number",
                        AttributeValue::String(_) => "String",
                        AttributeValue::Null => continue,
                    };
                    fields.push((k.clone(), t));
                }
            }
            fields.sort();
            let fields_json: Vec<String> =
                fields.iter().map(|(k, t)| format!("{k:?}: \"{t}\"")).collect();
            format!(
                "{{\"id\": {:?}, \"minzoom\": {}, \"maxzoom\": {}, \"fields\": {{{}}}}}",
                layer.name,
                opts.min_zoom,
                opts.max_zoom,
                fields_json.join(", ")
            )
        })
        .collect();
    format!("{{\"vector_layers\": [{}]}}", layers.join(", "))
}

#[cfg(feature = "parallel")]
fn run_tiles<S, F>(
    source: &VectorSource,
    opts: &MvtOptions,
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
            for (coord, data) in rx {
                sink.put(coord, &data)?;
                written += 1;
            }
            Ok(written)
        });

        let render_result: Result<()> = tiles.par_iter().try_for_each_init(
            || tx.clone(),
            |tx, &coord| {
                if let Some(data) = render_mvt_tile(source, opts, coord)? {
                    let _ = tx.send((coord, data));
                }
                progress(done.fetch_add(1, Ordering::Relaxed) + 1);
                Ok(())
            },
        );
        drop(tx);

        let written = writer.join().expect("tile writer thread panicked")?;
        render_result?;
        Ok(PyramidStats { written, skipped: tiles.len() as u64 - written })
    })
}

#[cfg(not(feature = "parallel"))]
fn run_tiles<S, F>(
    source: &VectorSource,
    opts: &MvtOptions,
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
        match render_mvt_tile(source, opts, coord)? {
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

/// Render one MVT tile; `None` when no feature intersects it.
fn render_mvt_tile(
    source: &VectorSource,
    opts: &MvtOptions,
    coord: TileCoord,
) -> Result<Option<Vec<u8>>> {
    let (min_x, min_y, max_x, max_y) = coord.bounds_meters();
    let span = max_x - min_x;
    let buffer_m = span * opts.buffer as f64 / opts.extent as f64;
    let clip_rect = Rect::new(
        geo_types::Coord { x: min_x - buffer_m, y: min_y - buffer_m },
        geo_types::Coord { x: max_x + buffer_m, y: max_y + buffer_m },
    );
    // Tolerance proportional to one tile unit at this zoom.
    let tol_m = span / opts.extent as f64 * opts.simplify;

    // Mercator meters → tile units (y grows downward).
    let scale = opts.extent as f64 / span;
    let qx = |x: f64| (x - min_x) * scale;
    let qy = |y: f64| (max_y - y) * scale;

    let mut tile = MvtTile::new(opts.extent);
    let mut any_feature = false;

    for layer_def in source.layers() {
        let mut layer = tile.create_layer(&layer_def.name);
        let mut layer_used = false;

        for feature in &layer_def.features {
            if !bbox_intersects(feature.bbox, &clip_rect) {
                continue;
            }
            let Some((geom_type, parts)) = clip_quantize(feature, &clip_rect, tol_m, qx, qy)
            else {
                continue;
            };
            // Canonical encoder idiom: complete_geom() *between* parts;
            // encode() finalizes the last one. Calling it after every part
            // (then encode()) would double-complete the final geometry.
            let mut enc = GeomEncoder::new(geom_type);
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    enc.complete_geom()
                        .map_err(|e| Error::Encode(format!("mvt geometry: {e:?}")))?;
                }
                for &(x, y) in part {
                    enc.add_point(x as f64, y as f64)
                        .map_err(|e| Error::Encode(format!("mvt geometry: {e:?}")))?;
                }
            }
            let data = enc.encode().map_err(|e| Error::Encode(format!("mvt: {e:?}")))?;
            if data.is_empty() {
                continue;
            }
            let mut mf = layer.into_feature(data);
            if let Some(id) = feature.id {
                mf.set_id(id);
            }
            for (k, v) in &feature.properties {
                match v {
                    AttributeValue::Bool(b) => mf.add_tag_bool(k, *b),
                    AttributeValue::Int(i) => mf.add_tag_int(k, *i),
                    AttributeValue::Float(f) => mf.add_tag_double(k, *f),
                    AttributeValue::String(s) => mf.add_tag_string(k, s),
                    AttributeValue::Null => {}
                }
            }
            layer = mf.into_layer();
            layer_used = true;
        }

        if layer_used {
            tile.add_layer(layer)
                .map_err(|e| Error::Encode(format!("mvt layer: {e:?}")))?;
            any_feature = true;
        }
    }

    if !any_feature {
        return Ok(None);
    }
    let bytes = tile.to_bytes().map_err(|e| Error::Encode(format!("mvt tile: {e:?}")))?;
    if !opts.compress {
        return Ok(Some(bytes));
    }
    let mut gz = GzEncoder::new(Vec::new(), Flate::default());
    gz.write_all(&bytes)
        .and_then(|_| gz.finish())
        .map(Some)
        .map_err(|e| Error::Encode(format!("gzip: {e}")))
}

/// MVT geometry type plus its parts in integer tile coordinates
/// (rings for polygons, lines for linestrings, point runs for points).
type QuantizedGeom = (GeomType, Vec<Vec<(i64, i64)>>);

fn bbox_intersects(b: (f64, f64, f64, f64), rect: &Rect<f64>) -> bool {
    b.0 <= rect.max().x && b.2 >= rect.min().x && b.1 <= rect.max().y && b.3 >= rect.min().y
}

/// Clip a feature to the rect, simplify, quantize to integer tile coords.
///
/// Returns the MVT geometry type plus its parts (rings/lines/point runs),
/// or `None` when the feature degenerates away at this zoom.
fn clip_quantize(
    feature: &VectorFeature,
    clip_rect: &Rect<f64>,
    tol_m: f64,
    qx: impl Fn(f64) -> f64,
    qy: impl Fn(f64) -> f64,
) -> Option<QuantizedGeom> {
    let quant = |c: geo_types::Coord<f64>| (qx(c.x).round() as i64, qy(c.y).round() as i64);

    match &feature.geometry {
        Geometry::Point(p) => {
            contains(clip_rect, p.0).then(|| (GeomType::Point, vec![vec![quant(p.0)]]))
        }
        Geometry::MultiPoint(mp) => {
            let pts: Vec<(i64, i64)> = mp
                .iter()
                .filter(|p| contains(clip_rect, p.0))
                .map(|p| quant(p.0))
                .collect();
            (!pts.is_empty()).then_some((GeomType::Point, vec![pts]))
        }
        Geometry::LineString(ls) => {
            quantize_lines(clip_lines(&MultiLineString(vec![ls.clone()]), clip_rect, tol_m), quant)
        }
        Geometry::MultiLineString(mls) => {
            quantize_lines(clip_lines(mls, clip_rect, tol_m), quant)
        }
        Geometry::Polygon(p) => {
            quantize_polygons(clip_polygons(&MultiPolygon(vec![p.clone()]), clip_rect, tol_m), quant)
        }
        Geometry::MultiPolygon(mp) => {
            quantize_polygons(clip_polygons(mp, clip_rect, tol_m), quant)
        }
        _ => None,
    }
}

fn contains(rect: &Rect<f64>, c: geo_types::Coord<f64>) -> bool {
    c.x >= rect.min().x && c.x <= rect.max().x && c.y >= rect.min().y && c.y <= rect.max().y
}

fn rect_polygon(rect: &Rect<f64>) -> Polygon<f64> {
    polygon![
        (x: rect.min().x, y: rect.min().y),
        (x: rect.max().x, y: rect.min().y),
        (x: rect.max().x, y: rect.max().y),
        (x: rect.min().x, y: rect.max().y),
        (x: rect.min().x, y: rect.min().y),
    ]
}

fn clip_lines(mls: &MultiLineString<f64>, rect: &Rect<f64>, tol_m: f64) -> MultiLineString<f64> {
    let clipped = rect_polygon(rect).clip(mls, false);
    if tol_m > 0.0 { clipped.simplify(&tol_m) } else { clipped }
}

fn clip_polygons(mp: &MultiPolygon<f64>, rect: &Rect<f64>, tol_m: f64) -> MultiPolygon<f64> {
    let clipped = MultiPolygon(vec![rect_polygon(rect)]).intersection(mp);
    if tol_m > 0.0 { clipped.simplify(&tol_m) } else { clipped }
}

fn quantize_lines(
    mls: MultiLineString<f64>,
    quant: impl Fn(geo_types::Coord<f64>) -> (i64, i64),
) -> Option<QuantizedGeom> {
    let mut parts = vec![];
    for ls in &mls {
        let pts = dedupe(ls.coords().map(|&c| quant(c)));
        if pts.len() >= 2 {
            parts.push(pts);
        }
    }
    avoid_shared_endpoints(&mut parts, false);
    (!parts.is_empty()).then_some((GeomType::Linestring, parts))
}

fn quantize_polygons(
    mp: MultiPolygon<f64>,
    quant: impl Fn(geo_types::Coord<f64>) -> (i64, i64),
) -> Option<QuantizedGeom> {
    let mut parts = vec![];
    for poly in &mp {
        let Some(ext) = quantize_ring(poly.exterior(), &quant, true) else {
            // Exterior collapsed: the whole polygon degenerates.
            continue;
        };
        parts.push(ext);
        for hole in poly.interiors() {
            if let Some(ring) = quantize_ring(hole, &quant, false) {
                parts.push(ring);
            }
        }
    }
    avoid_shared_endpoints(&mut parts, true);
    (!parts.is_empty()).then_some((GeomType::Polygon, parts))
}

/// Quantize a ring (open form — closing point dropped; the encoder adds
/// ClosePath) and normalize winding: MVT wants exterior rings with
/// positive signed area in tile coords (y down) and holes negative.
fn quantize_ring(
    ring: &LineString<f64>,
    quant: impl Fn(geo_types::Coord<f64>) -> (i64, i64),
    exterior: bool,
) -> Option<Vec<(i64, i64)>> {
    let coords = ring.coords();
    let mut pts = dedupe(coords.map(|&c| quant(c)));
    // geo rings repeat the first point at the end; drop it (open ring).
    if pts.len() >= 2 && pts.first() == pts.last() {
        pts.pop();
    }
    if pts.len() < 3 {
        return None;
    }
    // Shoelace on tile coords (y down): positive = clockwise on screen.
    let mut area2 = 0i64;
    for i in 0..pts.len() {
        let (x0, y0) = pts[i];
        let (x1, y1) = pts[(i + 1) % pts.len()];
        area2 += x0 * y1 - x1 * y0;
    }
    if area2 == 0 {
        return None;
    }
    if (area2 > 0) != exterior {
        pts.reverse();
    }
    Some(pts)
}

/// Ensure no part begins exactly where the previous one ended.
///
/// The `mvt` encoder keeps its last-point state across `complete_geom`, so
/// a new part whose first quantized point equals the previous part's last
/// point is mis-encoded as a continuation (a `LineTo` with no `MoveTo`),
/// which GDAL rejects. We break such collisions losslessly: lines are
/// reversed (direction-agnostic), rings rotated about their start (rings
/// are cyclic). A part that still collides afterwards is dropped.
fn avoid_shared_endpoints(parts: &mut Vec<Vec<(i64, i64)>>, ring: bool) {
    let mut i = 1;
    while i < parts.len() {
        let prev_last = *parts[i - 1].last().unwrap();
        if parts[i][0] != prev_last {
            i += 1;
            continue;
        }
        if ring {
            // Rotate the ring start until the first vertex differs.
            let mut rotated = 0;
            while parts[i][0] == prev_last && rotated < parts[i].len() {
                parts[i].rotate_left(1);
                rotated += 1;
            }
        } else {
            parts[i].reverse();
        }
        if parts[i][0] == prev_last {
            // Degenerate (collapsed to the shared point): drop it.
            parts.remove(i);
        } else {
            i += 1;
        }
    }
}

fn dedupe(iter: impl Iterator<Item = (i64, i64)>) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = vec![];
    for p in iter {
        if out.last() != Some(&p) {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mercator;
    use crate::vector::VectorSource;
    use geo_types::{LineString, Point, polygon};
    use std::collections::HashMap;
    use surtgis_core::vector::{Feature, FeatureCollection};

    struct MemSink {
        tiles: HashMap<(u8, u32, u32), Vec<u8>>,
        meta: Option<PyramidMetadata>,
    }

    impl TileSink for MemSink {
        fn put(&mut self, c: TileCoord, data: &[u8]) -> Result<()> {
            self.tiles.insert((c.z, c.x, c.y), data.to_vec());
            Ok(())
        }
        fn finalize(&mut self, meta: &PyramidMetadata) -> Result<()> {
            self.meta = Some(meta.clone());
            Ok(())
        }
    }

    fn source() -> VectorSource {
        let mut fc = FeatureCollection::new();
        let mut poly = Feature::new(Geometry::Polygon(polygon![
            (x: -71.6, y: -33.1),
            (x: -70.9, y: -33.1),
            (x: -70.9, y: -32.4),
            (x: -71.6, y: -32.4),
            (x: -71.6, y: -33.1),
        ]));
        poly.set_property("name", AttributeValue::String("zona".into()));
        fc.push(poly);
        let mut line = Feature::new(Geometry::LineString(LineString::from(vec![
            (-71.55, -33.05),
            (-71.0, -32.5),
        ])));
        line.id = Some("7".into());
        fc.push(line);
        fc.push(Feature::new(Geometry::Point(Point::new(-71.3, -32.8))));
        VectorSource::from_collection("capa", fc, None).unwrap()
    }

    #[test]
    fn generates_gzipped_pbf_tiles() {
        let opts = MvtOptions { min_zoom: 0, max_zoom: 8, ..Default::default() };
        let mut sink = MemSink { tiles: HashMap::new(), meta: None };
        let stats = generate_mvt(&source(), &opts, &mut sink, |_| {}).unwrap();
        assert!(stats.written >= 9, "one tile per zoom at least: {stats:?}");
        for data in sink.tiles.values() {
            assert_eq!(&data[..2], &[0x1f, 0x8b], "tiles must be gzip");
        }
        let meta = sink.meta.unwrap();
        assert_eq!(meta.format, "pbf");
        let json = meta.json.unwrap();
        assert!(json.contains("\"vector_layers\""), "{json}");
        assert!(json.contains("\"capa\""));
        assert!(json.contains("\"name\": \"String\""));
    }

    #[test]
    fn uncompressed_tiles_are_raw_protobuf() {
        let opts = MvtOptions {
            min_zoom: 6,
            max_zoom: 6,
            compress: false,
            ..Default::default()
        };
        let mut sink = MemSink { tiles: HashMap::new(), meta: None };
        generate_mvt(&source(), &opts, &mut sink, |_| {}).unwrap();
        // protobuf field 3 (layer), wire type 2 → first byte 0x1A.
        let data = sink.tiles.values().next().unwrap();
        assert_eq!(data[0], 0x1A, "expected raw MVT protobuf");
    }

    #[test]
    fn shared_endpoint_lines_are_separated() {
        // Two line parts that touch at a quantized point: the encoder would
        // otherwise merge them into invalid command runs.
        let mut parts = vec![vec![(0i64, 0i64), (10, 0)], vec![(10, 0), (10, 10)]];
        avoid_shared_endpoints(&mut parts, false);
        assert_ne!(parts[1][0], parts[0][parts[0].len() - 1]);
    }

    #[test]
    fn shared_endpoint_rings_are_rotated() {
        let mut parts = vec![
            vec![(0i64, 0i64), (10, 0), (10, 10), (5, 5)],
            vec![(5, 5), (2, 2), (3, 1)],
        ];
        avoid_shared_endpoints(&mut parts, true);
        assert_ne!(parts[1][0], parts[0][parts[0].len() - 1]);
        // Ring rotation preserves the vertex set.
        assert_eq!(parts[1].len(), 3);
    }

    #[test]
    fn winding_normalization() {
        // Counter-clockwise ring in tile coords (y down) must be reversed
        // for an exterior.
        let ring = LineString::from(vec![(0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 0.0), (0.0, 0.0)]);
        let pts = quantize_ring(&ring, |c| (c.x as i64, c.y as i64), true).unwrap();
        let mut area2 = 0i64;
        for i in 0..pts.len() {
            let (x0, y0) = pts[i];
            let (x1, y1) = pts[(i + 1) % pts.len()];
            area2 += x0 * y1 - x1 * y0;
        }
        assert!(area2 > 0, "exterior must end up clockwise (positive area)");
    }

    #[test]
    fn degenerate_ring_is_dropped() {
        let ring = LineString::from(vec![(0.0, 0.0), (0.2, 0.2), (0.4, 0.4), (0.0, 0.0)]);
        // Quantizes to collinear/duplicate points → dropped.
        assert!(quantize_ring(&ring, |c| (c.x.round() as i64, c.y.round() as i64), true).is_none());
    }

    #[test]
    fn tiny_polygon_vanishes_at_low_zoom_but_not_high() {
        // ~100 m square near Valparaíso.
        let (mx, my) = mercator::lonlat_to_meters(-71.3, -32.8);
        let mut fc = FeatureCollection::new();
        fc.push(Feature::new(Geometry::Polygon(polygon![
            (x: mx, y: my),
            (x: mx + 100.0, y: my),
            (x: mx + 100.0, y: my + 100.0),
            (x: mx, y: my + 100.0),
            (x: mx, y: my),
        ])));
        let src =
            VectorSource::from_collection("p", fc, Some(crate::source::SourceCrs::Mercator))
                .unwrap();

        let mut low = MemSink { tiles: HashMap::new(), meta: None };
        let low_stats = generate_mvt(
            &src,
            &MvtOptions { min_zoom: 2, max_zoom: 2, ..Default::default() },
            &mut low,
            |_| {},
        );
        // At z2 the square is far below one tile unit → tile skipped.
        assert!(low_stats.unwrap().written == 0);

        let mut high = MemSink { tiles: HashMap::new(), meta: None };
        let high_stats = generate_mvt(
            &src,
            &MvtOptions { min_zoom: 14, max_zoom: 14, ..Default::default() },
            &mut high,
            |_| {},
        )
        .unwrap();
        assert!(high_stats.written >= 1);
    }
}
