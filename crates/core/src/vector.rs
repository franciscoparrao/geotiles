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

    /// Read a FlatGeobuf file into a single named layer, keeping only the
    /// features intersecting `bbox` (in the file's own CRS).
    ///
    /// Uses the FGB R-tree index, so only matching features are decoded —
    /// useful over large files when tiling a sub-region.
    pub fn from_file_bbox(
        path: impl AsRef<Path>,
        layer_name: &str,
        bbox: (f64, f64, f64, f64),
        crs_override: Option<SourceCrs>,
    ) -> Result<Self> {
        let fc = read_flatgeobuf(path.as_ref(), Some(bbox))?;
        Self::from_collection(layer_name, fc, crs_override)
    }

    /// Like [`VectorSource::from_file_bbox`], but appends to an existing
    /// source as another named layer.
    pub fn push_file_bbox(
        &mut self,
        path: impl AsRef<Path>,
        layer_name: &str,
        bbox: (f64, f64, f64, f64),
        crs_override: Option<SourceCrs>,
    ) -> Result<()> {
        if self.layers.iter().any(|l| l.name == layer_name) {
            return Err(Error::InvalidInput(format!(
                "duplicate layer name {layer_name:?}"
            )));
        }
        let fc = read_flatgeobuf(path.as_ref(), Some(bbox))?;
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
///
/// surtgis-core v1.0+ handles GeoJSON with all geometry types (including
/// LineString and MultiPoint), so the custom parser is no longer needed.
/// v1.1 adds Shapefile and full-geometry GeoParquet readers; v0.4 of
/// geotiles adds FlatGeobuf (streaming, via geozero → geo-types).
fn read_file(path: &Path, gpkg_layer: Option<&str>) -> Result<FeatureCollection> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "geojson" | "json" => Ok(surtgis_core::vector::read_geojson(path)?),
        "gpkg" => Ok(surtgis_core::vector::read_gpkg(path, gpkg_layer)?),
        "shp" => Ok(surtgis_core::vector::read_shapefile(path)?),
        "parquet" => Ok(surtgis_core::vector::read_geoparquet(path)?),
        "fgb" => read_flatgeobuf(path, None),
        _ => Ok(surtgis_core::vector::read_vector(path)?),
    }
}

/// Read a FlatGeobuf file into a `FeatureCollection`.
///
/// Streams features via the `flatgeobuf` crate. Each feature's geometry is
/// decoded with geozero's `GeoWriter` (→ `geo_types::Geometry`) and its
/// properties via a small `PropertyProcessor`. The file's CRS is read from
/// the FGB header when it carries an EPSG code.
fn read_flatgeobuf(path: &Path, bbox: Option<(f64, f64, f64, f64)>) -> Result<FeatureCollection> {
    use fallible_streaming_iterator::FallibleStreamingIterator;
    use geozero::geo_types::GeoWriter;
    use geozero::{FeatureProperties, GeozeroGeometry};
    use std::io::BufReader;

    let file = std::fs::File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let reader = flatgeobuf::FgbReader::open(BufReader::new(file))
        .map_err(|e| Error::InvalidInput(format!("{}: {e}", path.display())))?;
    // A bbox filter uses the file's R-tree (in the file's own CRS), so only
    // features intersecting the box are decoded — streaming over large files.
    let mut fgb = match bbox {
        Some((min_x, min_y, max_x, max_y)) => reader
            .select_bbox(min_x, min_y, max_x, max_y)
            .map_err(|e| Error::InvalidInput(format!("{}: {e}", path.display())))?,
        None => reader
            .select_all()
            .map_err(|e| Error::InvalidInput(format!("{}: {e}", path.display())))?,
    };

    // CRS from the FGB header (EPSG code when available).
    let header = fgb.header();
    let crs = header
        .crs()
        .map(|c| surtgis_core::CRS::from_epsg(c.code() as u32));
    let mut fc = FeatureCollection::with_crs(crs);

    let mut props = PropertyAccumulator::default();
    while let Some(feature) = fgb
        .next()
        .map_err(|e| Error::InvalidInput(format!("{}: {e}", path.display())))?
    {
        let mut gw = GeoWriter::new();
        feature
            .process_geom(&mut gw)
            .map_err(|e| Error::InvalidInput(format!("{}: {e}", path.display())))?;
        let Some(geometry) = gw.take_geometry() else {
            continue;
        };
        props.clear();
        feature
            .process_properties(&mut props)
            .map_err(|e| Error::InvalidInput(format!("{}: {e}", path.display())))?;

        let mut f = surtgis_core::vector::Feature::new(geometry);
        for (name, value) in props.take() {
            f.set_property(name, fgb_value_to_attribute(&value));
        }
        fc.push(f);
    }
    Ok(fc)
}

/// Collects feature properties as they stream through a `PropertyProcessor`.
#[derive(Default)]
struct PropertyAccumulator {
    map: Vec<(String, geozero::geo_types::OwnedColumnValue)>,
}

impl PropertyAccumulator {
    fn clear(&mut self) {
        self.map.clear();
    }
    fn take(&mut self) -> Vec<(String, geozero::geo_types::OwnedColumnValue)> {
        std::mem::take(&mut self.map)
    }
}

impl geozero::PropertyProcessor for PropertyAccumulator {
    fn property(
        &mut self,
        _idx: usize,
        name: &str,
        value: &geozero::ColumnValue,
    ) -> geozero::error::Result<bool> {
        use geozero::geo_types::OwnedColumnValue;
        self.map.push((name.to_string(), OwnedColumnValue::from(value)));
        // false = keep processing remaining properties.
        Ok(false)
    }
}

/// Map a geozero `OwnedColumnValue` to surtgis' `AttributeValue`.
fn fgb_value_to_attribute(v: &geozero::geo_types::OwnedColumnValue) -> AttributeValue {
    use geozero::geo_types::OwnedColumnValue as V;
    match v {
        V::Byte(x) => AttributeValue::Int(*x as i64),
        V::UByte(x) => AttributeValue::Int(*x as i64),
        V::Short(x) => AttributeValue::Int(*x as i64),
        V::UShort(x) => AttributeValue::Int(*x as i64),
        V::Int(x) => AttributeValue::Int(*x as i64),
        V::UInt(x) => AttributeValue::Int(*x as i64),
        V::Long(x) => AttributeValue::Int(*x),
        V::ULong(x) => AttributeValue::Float(*x as f64),
        V::Bool(x) => AttributeValue::Bool(*x),
        V::Float(x) => AttributeValue::Float(*x as f64),
        V::Double(x) => AttributeValue::Float(*x),
        V::String(s) | V::Json(s) | V::DateTime(s) => AttributeValue::String(s.clone()),
        V::Binary(_) => AttributeValue::Null,
    }
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
    fn reads_shapefile_input() {
        // Build a .shp fixture with the shapefile crate (as surtgis tests do).
        use shapefile::dbase::{FieldName, FieldValue, TableWriterBuilder};
        use shapefile::{Point, PolygonRing, Writer};

        let dir = tempfile::tempdir().unwrap();
        let shp = dir.path().join("cuenca.shp");
        let polygon = shapefile::Polygon::with_rings(vec![PolygonRing::Outer(vec![
            Point::new(-71.5, -33.0),
            Point::new(-71.0, -33.0),
            Point::new(-71.0, -32.5),
            Point::new(-71.5, -32.5),
            Point::new(-71.5, -33.0),
        ])]);
        let table_builder = TableWriterBuilder::new()
            .add_character_field(FieldName::try_from("name").unwrap(), 50);
        let mut writer = Writer::from_path(&shp, table_builder).unwrap();
        let mut record = shapefile::dbase::Record::default();
        record.insert(
            "name".to_string(),
            FieldValue::Character(Some("Cuenca1".to_string())),
        );
        writer.write_shape_and_record(&polygon, &record).unwrap();
        drop(writer);

        // Without .prj the CRS must be inferred from the extent (lon/lat here).
        let src = VectorSource::from_file(&shp, "cuencas", None, None).unwrap();
        let layer = &src.layers()[0];
        assert_eq!(layer.name, "cuencas");
        assert_eq!(layer.features.len(), 1);
        // The shapefile crate normalizes polygons to MultiPolygon.
        assert!(matches!(layer.features[0].geometry, Geometry::MultiPolygon(_)));
        let props = &layer.features[0].properties;
        assert!(props.iter().any(|(k, v)| k == "name"
            && matches!(v, AttributeValue::String(s) if s == "Cuenca1")));
        // Reprojected to mercator → x is very negative.
        let (x0, _, _, _) = src.bounds_meters();
        assert!(x0 < -7.9e6, "expected mercator, got {x0}");
    }

    #[test]
    fn reads_geoparquet_input() {
        // surtgis' GeoParquet writer is point-only, so hand-write a
        // LineString fixture using the parquet crate (dev-dependency).
        use std::sync::Arc;
        use parquet::basic::Repetition;
        use parquet::data_type::ByteArray;
        use parquet::file::metadata::KeyValue;
        use parquet::file::properties::WriterProperties;
        use parquet::file::writer::SerializedFileWriter;
        use parquet::schema::types::Type as SchemaType;
        use parquet::data_type::ByteArrayType;
        use parquet::basic::Compression;

        // WKB LineString in lon/lat: LINESTRING(-71.4 -32.9, -71.2 -32.7)
        let mut wkb = vec![1u8, 2, 0, 0, 0, 2, 0, 0, 0];
        for (x, y) in [(-71.4f64, -32.9f64), (-71.2f64, -32.7f64)] {
            wkb.extend_from_slice(&x.to_le_bytes());
            wkb.extend_from_slice(&y.to_le_bytes());
        }
        let geo_meta = r#"{"version":"1.0.0","primary_column":"geometry",
               "columns":{"geometry":{"encoding":"WKB",
               "geometry_types":["LineString"],
               "crs":"http://www.opengis.net/def/crs/EPSG/0/4326"}}}"#
            .to_string();

        let schema = Arc::new(
            SchemaType::group_type_builder("schema")
                .with_fields(vec![Arc::new(
                    SchemaType::primitive_type_builder("geometry", parquet::basic::Type::BYTE_ARRAY)
                        .with_repetition(Repetition::REQUIRED)
                        .build()
                        .unwrap(),
                )])
                .build()
                .unwrap(),
        );
        let props = Arc::new(
            WriterProperties::builder()
                .set_compression(Compression::SNAPPY)
                .set_key_value_metadata(Some(vec![KeyValue::new(
                    "geo".to_string(),
                    geo_meta,
                )]))
                .build(),
        );

        let dir = tempfile::tempdir().unwrap();
        let pq = dir.path().join("red.parquet");
        let file = std::fs::File::create(&pq).unwrap();
        let mut writer = SerializedFileWriter::new(file, schema, props).unwrap();
        let mut rg = writer.next_row_group().unwrap();
        let mut col = rg.next_column().unwrap().unwrap();
        col.typed::<ByteArrayType>()
            .write_batch(&[ByteArray::from(wkb)], None, None)
            .unwrap();
        col.close().unwrap();
        rg.close().unwrap();
        writer.close().unwrap();

        let src = VectorSource::from_file(&pq, "red", None, None).unwrap();
        let layer = &src.layers()[0];
        assert_eq!(layer.features.len(), 1);
        assert!(matches!(layer.features[0].geometry, Geometry::LineString(_)));
        // Reprojected to mercator.
        let (x0, _, _, _) = src.bounds_meters();
        assert!(x0 < -7.9e6, "expected mercator, got {x0}");
    }

    #[test]
    fn reads_geoparquet_with_nullable_columns() {
        // surtgis-core 1.2.0 tolerates nulls in GeoParquet attribute
        // columns (geopandas writes these by default). Hand-write a fixture
        // with one OPTIONAL string column where the first row is null.
        use std::sync::Arc;
        use parquet::basic::Repetition;
        use parquet::data_type::{ByteArray, ByteArrayType};
        use parquet::file::metadata::KeyValue;
        use parquet::file::properties::WriterProperties;
        use parquet::file::writer::SerializedFileWriter;
        use parquet::schema::types::Type as SchemaType;
        use parquet::basic::Compression;

        // WKB Point in lon/lat.
        fn wkb_point(x: f64, y: f64) -> Vec<u8> {
            let mut buf = vec![1u8, 1, 0, 0, 0];
            buf.extend_from_slice(&x.to_le_bytes());
            buf.extend_from_slice(&y.to_le_bytes());
            buf
        }
        let geo_meta = r#"{"version":"1.0.0","primary_column":"geometry",
               "columns":{"geometry":{"encoding":"WKB",
               "geometry_types":["Point"],
               "crs":"http://www.opengis.net/def/crs/EPSG/0/4326"}}}"#
            .to_string();

        let schema = Arc::new(
            SchemaType::group_type_builder("schema")
                .with_fields(vec![
                    Arc::new(
                        SchemaType::primitive_type_builder(
                            "geometry",
                            parquet::basic::Type::BYTE_ARRAY,
                        )
                        .with_repetition(Repetition::REQUIRED)
                        .build()
                        .unwrap(),
                    ),
                    Arc::new(
                        SchemaType::primitive_type_builder(
                            "estacion",
                            parquet::basic::Type::BYTE_ARRAY,
                        )
                        .with_logical_type(Some(parquet::basic::LogicalType::String))
                        .with_converted_type(parquet::basic::ConvertedType::UTF8)
                        .with_repetition(Repetition::OPTIONAL)
                        .build()
                        .unwrap(),
                    ),
                ])
                .build()
                .unwrap(),
        );
        let props = Arc::new(
            WriterProperties::builder()
                .set_compression(Compression::SNAPPY)
                .set_key_value_metadata(Some(vec![KeyValue::new(
                    "geo".to_string(),
                    geo_meta,
                )]))
                .build(),
        );

        let dir = tempfile::tempdir().unwrap();
        let pq = dir.path().join("puntos_nulls.parquet");
        let file = std::fs::File::create(&pq).unwrap();
        let mut writer = SerializedFileWriter::new(file, schema, props).unwrap();
        let mut rg = writer.next_row_group().unwrap();

        // Column 0: two point geometries.
        let mut col = rg.next_column().unwrap().unwrap();
        col.typed::<ByteArrayType>()
            .write_batch(
                &[ByteArray::from(wkb_point(-71.0, -33.0)), ByteArray::from(wkb_point(-71.1, -33.1))],
                None,
                None,
            )
            .unwrap();
        col.close().unwrap();

        // Column 1: OPTIONAL string — first row null, second row "E01".
        // Nulls don't consume a slot in the values batch, so only the
        // present value is written, with a definition-level array [0,1].
        let mut col = rg.next_column().unwrap().unwrap();
        col.typed::<ByteArrayType>()
            .write_batch(
                &[ByteArray::from("E01")],
                Some(&[0i16, 1i16]),
                None,
            )
            .unwrap();
        col.close().unwrap();

        rg.close().unwrap();
        writer.close().unwrap();

        let src = VectorSource::from_file(&pq, "puntos", None, None).unwrap();
        let layer = &src.layers()[0];
        assert_eq!(layer.features.len(), 2);
        // First feature carries AttributeValue::Null for `estacion`.
        let null_feature = &layer.features[0];
        assert!(null_feature.properties.iter().any(
            |(k, v)| k == "estacion" && matches!(v, AttributeValue::Null)
        ));
        let named = &layer.features[1];
        assert!(named.properties.iter().any(
            |(k, v)| k == "estacion" && matches!(v, AttributeValue::String(s) if s == "E01")
        ));
    }

    #[test]
    fn reads_flatgeobuf_input() {
        // Reads a GDAL-produced .fgb fixture (tests/fixtures/cuencas.fgb).
        // The flatgeobuf crate's own writer is buggy for properties (its
        // column registration drops feature values — GDAL rejects those
        // files), so the fixture comes from GDAL, the reference writer.
        let fgb_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cuencas.fgb");

        let src = VectorSource::from_file(&fgb_path, "cuencas", None, None).unwrap();
        let layer = &src.layers()[0];
        assert_eq!(layer.name, "cuencas");
        assert_eq!(layer.features.len(), 1);
        // GDAL's writer may keep it as Polygon or promote to MultiPolygon.
        assert!(matches!(
            layer.features[0].geometry,
            Geometry::Polygon(_) | Geometry::MultiPolygon(_)
        ));
        let props = &layer.features[0].properties;
        assert!(props.iter().any(
            |(k, v)| k == "name" && matches!(v, AttributeValue::String(s) if s == "Cuenca1")
        ));
        assert!(props.iter().any(
            |(k, v)| k == "area_km2" && matches!(v, AttributeValue::Float(x) if (*x - 12.5).abs() < 1e-9)
        ));
        // Reprojected to mercator.
        let (x0, _, _, _) = src.bounds_meters();
        assert!(x0 < -7.9e6, "expected mercator, got {x0}");
    }

    #[test]
    fn flatgeobuf_bbox_filter_keeps_intersecting_features() {
        // pts_bbox.fgb has 5 points: 2 near lon -72..-72.5 (west), 3 near
        // lon -70.5..-71 (east). A bbox over the east half must return only
        // those 3.
        let fgb_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pts_bbox.fgb");

        let src =
            VectorSource::from_file_bbox(&fgb_path, "pts", (-71.2, -33.2, -70.4, -32.4), None)
                .unwrap();
        let layer = &src.layers()[0];
        assert_eq!(layer.features.len(), 3, "bbox filter should keep east points");
        for f in &layer.features {
            // In lon/lat source: x (west) should be > -71.2 (mercator).
            assert!(
                f.bbox.0 > mercator::lonlat_to_meters(-71.2, 0.0).0,
                "kept a west feature: {:?}",
                f.bbox
            );
        }
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
