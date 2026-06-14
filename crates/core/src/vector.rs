//! Vector sources for MVT tiling.
//!
//! A [`VectorSource`] holds one or more named layers of features already
//! reprojected to Web Mercator. v0.2's CLI fills a single layer per run,
//! but the model (and the MVT encoder) is multi-layer from the start so
//! multi-layer tilesets only need a CLI extension.

use std::path::Path;

use geo::BoundingRect;
use geo::MapCoords;
use geo_types::{Coord, Geometry};
use surtgis_core::vector::{AttributeValue, FeatureCollection};

use crate::error::{Error, Result};
use crate::mercator;
use crate::source::SourceCrs;

/// One feature, geometry in Web Mercator meters.
#[derive(Debug, Clone)]
pub struct VectorFeature {
    pub geometry: Geometry<f64>,
    /// Cached bbox `(min_x, min_y, max_x, max_y)` in mercator meters.
    pub bbox: (f64, f64, f64, f64),
    /// MVT feature id (only numeric source ids survive).
    pub id: Option<u64>,
    /// Properties, sorted by key for deterministic output.
    pub properties: Vec<(String, AttributeValue)>,
}

/// A named layer of features.
#[derive(Debug, Clone)]
pub struct VectorLayer {
    pub name: String,
    pub features: Vec<VectorFeature>,
}

/// One or more layers ready to be tiled.
#[derive(Debug)]
pub struct VectorSource {
    layers: Vec<VectorLayer>,
}

impl VectorSource {
    /// Read a vector file (GeoJSON, GPKG, …) into a single named layer.
    ///
    /// `gpkg_layer` selects the table for GeoPackage inputs (default:
    /// first layer). CRS handling: GeoJSON is lon/lat by spec; for other
    /// inputs a bounds heuristic applies unless `crs_override` is given.
    pub fn from_file(
        path: impl AsRef<Path>,
        layer_name: &str,
        gpkg_layer: Option<&str>,
        crs_override: Option<SourceCrs>,
    ) -> Result<Self> {
        let fc = read_file(path.as_ref(), gpkg_layer)?;
        Self::from_collection(layer_name, fc, crs_override)
    }

    /// Build a single-layer source from an in-memory collection.
    pub fn from_collection(
        layer_name: &str,
        fc: FeatureCollection,
        crs_override: Option<SourceCrs>,
    ) -> Result<Self> {
        let layer = build_layer(layer_name, fc, crs_override)?;
        if layer.features.is_empty() {
            return Err(Error::InvalidInput("no tileable features in input".into()));
        }
        Ok(Self { layers: vec![layer] })
    }

    /// Add another layer (multi-layer tilesets).
    pub fn push_layer(
        &mut self,
        layer_name: &str,
        fc: FeatureCollection,
        crs_override: Option<SourceCrs>,
    ) -> Result<()> {
        self.layers.push(build_layer(layer_name, fc, crs_override)?);
        Ok(())
    }

    /// Read a file and add it as another named layer.
    ///
    /// Rejects a layer name already present so a tileset never has two
    /// layers with the same `id` (which would be invalid MVT metadata).
    pub fn push_file(
        &mut self,
        path: impl AsRef<Path>,
        layer_name: &str,
        gpkg_layer: Option<&str>,
        crs_override: Option<SourceCrs>,
    ) -> Result<()> {
        if self.layers.iter().any(|l| l.name == layer_name) {
            return Err(Error::InvalidInput(format!(
                "duplicate layer name {layer_name:?}"
            )));
        }
        let fc = read_file(path.as_ref(), gpkg_layer)?;
        self.push_layer(layer_name, fc, crs_override)
    }

    /// The layers, in encoding order.
    pub fn layers(&self) -> &[VectorLayer] {
        &self.layers
    }

    /// Union of all feature bboxes, in mercator meters.
    pub fn bounds_meters(&self) -> (f64, f64, f64, f64) {
        let mut b = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for f in self.layers.iter().flat_map(|l| &l.features) {
            b.0 = b.0.min(f.bbox.0);
            b.1 = b.1.min(f.bbox.1);
            b.2 = b.2.max(f.bbox.2);
            b.3 = b.3.max(f.bbox.3);
        }
        b
    }

    /// Bounds in lon/lat degrees `(west, south, east, north)`.
    pub fn bounds_lonlat(&self) -> (f64, f64, f64, f64) {
        let (x0, y0, x1, y1) = self.bounds_meters();
        let (w, s) = mercator::meters_to_lonlat(x0, y0);
        let (e, n) = mercator::meters_to_lonlat(x1, y1);
        (w, s, e, n)
    }
}

/// Read any supported vector file into a `FeatureCollection`, dispatching
/// by extension. `gpkg_layer` selects a GeoPackage table (default: first).
fn read_file(path: &Path, gpkg_layer: Option<&str>) -> Result<FeatureCollection> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "gpkg" => Ok(surtgis_core::vector::read_gpkg(path, gpkg_layer)?),
        // Own GeoJSON reader: surtgis' parser lacks line geometries.
        "geojson" | "json" => read_geojson(path),
        _ => Ok(surtgis_core::vector::read_vector(path)?),
    }
}

/// Spec-complete GeoJSON reader (all geometry types) on the `geojson`
/// crate, normalized to surtgis' `FeatureCollection` model.
fn read_geojson(path: &Path) -> Result<FeatureCollection> {
    let text = std::fs::read_to_string(path)
        .map_err(|source| Error::Io { path: path.to_path_buf(), source })?;
    let gj: geojson::GeoJson = text
        .parse()
        .map_err(|e| Error::InvalidInput(format!("{}: {e}", path.display())))?;

    let features = match gj {
        geojson::GeoJson::FeatureCollection(fc) => fc.features,
        geojson::GeoJson::Feature(f) => vec![f],
        geojson::GeoJson::Geometry(g) => vec![geojson::Feature {
            bbox: None,
            geometry: Some(g),
            id: None,
            properties: None,
            foreign_members: None,
        }],
    };

    let mut out = FeatureCollection::new();
    for f in features {
        let geometry = f
            .geometry
            .and_then(|g| Geometry::<f64>::try_from(g).ok());
        let mut feature = match geometry {
            Some(g) => surtgis_core::vector::Feature::new(g),
            None => continue,
        };
        feature.id = f.id.map(|id| match id {
            geojson::feature::Id::String(s) => s,
            geojson::feature::Id::Number(n) => n.to_string(),
        });
        for (k, v) in f.properties.into_iter().flatten() {
            let attr = match v {
                serde_json::Value::Bool(b) => AttributeValue::Bool(b),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        AttributeValue::Int(i)
                    } else {
                        AttributeValue::Float(n.as_f64().unwrap_or(f64::NAN))
                    }
                }
                serde_json::Value::String(s) => AttributeValue::String(s),
                serde_json::Value::Null => AttributeValue::Null,
                // Arrays/objects degrade to their JSON text.
                other => AttributeValue::String(other.to_string()),
            };
            feature.set_property(k, attr);
        }
        out.push(feature);
    }
    Ok(out)
}

fn build_layer(
    name: &str,
    fc: FeatureCollection,
    crs_override: Option<SourceCrs>,
) -> Result<VectorLayer> {
    // Detect CRS from the data extent unless overridden: coordinates
    // within ±180/±90 read as degrees, anything mercator-sized as meters.
    let crs = match crs_override {
        Some(crs) => crs,
        None => detect_crs(&fc)?,
    };

    let mut features = Vec::with_capacity(fc.features.len());
    for f in fc.features {
        let Some(geom) = f.geometry else { continue };
        let Some(geom) = to_mercator(geom, crs) else { continue };
        let Some(rect) = geom.bounding_rect() else { continue };
        let id = f.id.as_deref().and_then(|s| s.parse().ok());
        let mut properties: Vec<(String, AttributeValue)> = f.properties.into_iter().collect();
        properties.sort_by(|a, b| a.0.cmp(&b.0));
        features.push(VectorFeature {
            geometry: geom,
            bbox: (rect.min().x, rect.min().y, rect.max().x, rect.max().y),
            id,
            properties,
        });
    }
    Ok(VectorLayer { name: name.to_string(), features })
}

fn detect_crs(fc: &FeatureCollection) -> Result<SourceCrs> {
    let mut max_abs_x: f64 = 0.0;
    let mut max_abs_y: f64 = 0.0;
    for f in fc.iter() {
        if let Some(rect) = f.geometry.as_ref().and_then(|g| g.bounding_rect()) {
            max_abs_x = max_abs_x.max(rect.min().x.abs()).max(rect.max().x.abs());
            max_abs_y = max_abs_y.max(rect.min().y.abs()).max(rect.max().y.abs());
        }
    }
    if max_abs_x <= 180.5 && max_abs_y <= 90.5 {
        Ok(SourceCrs::LonLat)
    } else if max_abs_x <= mercator::ORIGIN_SHIFT_M * 1.001
        && max_abs_y <= mercator::ORIGIN_SHIFT_M * 1.001
    {
        Ok(SourceCrs::Mercator)
    } else {
        Err(Error::InvalidInput(
            "cannot infer vector CRS from coordinates; pass an explicit override".into(),
        ))
    }
}

/// Reproject to mercator, keeping only MVT-encodable geometry kinds.
fn to_mercator(geom: Geometry<f64>, crs: SourceCrs) -> Option<Geometry<f64>> {
    let geom = match geom {
        g @ (Geometry::Point(_)
        | Geometry::MultiPoint(_)
        | Geometry::LineString(_)
        | Geometry::MultiLineString(_)
        | Geometry::Polygon(_)
        | Geometry::MultiPolygon(_)) => g,
        // Rare geo types normalize to the basic six.
        Geometry::Line(l) => Geometry::LineString(vec![l.start, l.end].into()),
        Geometry::Rect(r) => Geometry::Polygon(r.to_polygon()),
        Geometry::Triangle(t) => Geometry::Polygon(t.to_polygon()),
        Geometry::GeometryCollection(_) => return None,
    };
    Some(match crs {
        SourceCrs::Mercator => geom,
        SourceCrs::LonLat => geom.map_coords(|Coord { x, y }| {
            let (mx, my) = mercator::lonlat_to_meters(x, y);
            Coord { x: mx, y: my }
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo_types::{LineString, Point, Polygon, polygon};
    use surtgis_core::vector::Feature;

    fn collection() -> FeatureCollection {
        let mut fc = FeatureCollection::new();

        let mut poly = Feature::new(Geometry::Polygon(polygon![
            (x: -71.5, y: -33.0),
            (x: -71.0, y: -33.0),
            (x: -71.0, y: -32.5),
            (x: -71.5, y: -33.0),
        ]));
        poly.set_property("name", AttributeValue::String("cuenca".into()));
        poly.set_property("area_km2", AttributeValue::Float(12.5));
        fc.push(poly);

        let mut line = Feature::new(Geometry::LineString(LineString::from(vec![
            (-71.4, -32.9),
            (-71.2, -32.7),
        ])));
        line.id = Some("42".into());
        fc.push(line);

        fc.push(Feature::new(Geometry::Point(Point::new(-71.3, -32.8))));
        fc
    }

    #[test]
    fn builds_single_layer_in_mercator() {
        let src = VectorSource::from_collection("capa", collection(), None).unwrap();
        assert_eq!(src.layers().len(), 1);
        let layer = &src.layers()[0];
        assert_eq!(layer.name, "capa");
        assert_eq!(layer.features.len(), 3);

        // Coordinates are mercator meters now (≈ -7.96e6 for -71.5°).
        let (x0, _, x1, _) = src.bounds_meters();
        assert!(x0 < -7.9e6 && x1 < -7.9e6, "expected mercator: {x0}..{x1}");
        // Round-trip back to degrees.
        let (w, s, e, n) = src.bounds_lonlat();
        assert!((w - -71.5).abs() < 1e-9 && (e - -71.0).abs() < 1e-9);
        assert!(s < n);
    }

    #[test]
    fn numeric_id_survives_and_props_sorted() {
        let src = VectorSource::from_collection("capa", collection(), None).unwrap();
        let layer = &src.layers()[0];
        assert_eq!(layer.features[1].id, Some(42));
        let keys: Vec<&str> =
            layer.features[0].properties.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["area_km2", "name"]);
    }

    #[test]
    fn mercator_input_passes_through() {
        let mut fc = FeatureCollection::new();
        let (mx, my) = mercator::lonlat_to_meters(-71.0, -33.0);
        fc.push(Feature::new(Geometry::Point(Point::new(mx, my))));
        let src = VectorSource::from_collection("m", fc, None).unwrap();
        let (w, _, _, _) = src.bounds_lonlat();
        assert!((w - -71.0).abs() < 1e-6);
    }

    #[test]
    fn multi_layer_via_push() {
        let mut src = VectorSource::from_collection("a", collection(), None).unwrap();
        src.push_layer("b", collection(), None).unwrap();
        assert_eq!(src.layers().len(), 2);
        assert_eq!(src.layers()[1].name, "b");
    }

    #[test]
    fn push_file_reads_and_rejects_duplicate_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pts.geojson");
        std::fs::write(
            &path,
            r#"{"type":"FeatureCollection","features":[
                {"type":"Feature","properties":{"k":1},
                 "geometry":{"type":"Point","coordinates":[-71.0,-33.0]}}]}"#,
        )
        .unwrap();

        let mut src = VectorSource::from_collection("base", collection(), None).unwrap();
        src.push_file(&path, "puntos", None, None).unwrap();
        assert_eq!(src.layers().len(), 2);
        assert_eq!(src.layers()[1].name, "puntos");

        // Same name twice → rejected.
        assert!(src.push_file(&path, "puntos", None, None).is_err());
    }

    #[test]
    fn rejects_empty_input() {
        assert!(VectorSource::from_collection("x", FeatureCollection::new(), None).is_err());
    }

    #[test]
    fn rect_normalizes_to_polygon() {
        let mut fc = FeatureCollection::new();
        fc.push(Feature::new(Geometry::Rect(geo_types::Rect::new(
            Coord { x: -71.0, y: -33.0 },
            Coord { x: -70.0, y: -32.0 },
        ))));
        let src = VectorSource::from_collection("r", fc, None).unwrap();
        assert!(matches!(src.layers()[0].features[0].geometry, Geometry::Polygon(_)));
        let _: &Polygon<f64> = match &src.layers()[0].features[0].geometry {
            Geometry::Polygon(p) => p,
            _ => unreachable!(),
        };
    }
}
