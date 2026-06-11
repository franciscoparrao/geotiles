# Evaluación: Vector Tiles (MVT) para geotiles v0.2

> **Estado:** PROPUESTA (evaluación 2026-06-11). Pendiente GO del autor.
> **Conclusión corta:** GO acotado. El ecosistema Rust cubre todas las
> piezas; el costo está en clipping/simplificación por zoom, no en el
> encoding. Estimado: ~600–900 LOC en 2 etapas.

## 1. Recomendación

Implementar MVT en v0.2 con alcance acotado (sección 5), reutilizando
los lectores vectoriales de surtgis-core y el `TileSink` existente.
No intentar paridad con tippecanoe: replicar su núcleo (tiling +
clipping + simplificación + cuantización) y dejar fuera sus heurísticas
de feature-dropping, que son la mayor parte de su complejidad y solo
importan para datasets nacionales/planetarios.

## 2. Ecosistema (verificado en crates.io, 2026-06-11)

| Crate | Versión | Rol | Notas |
|---|---|---|---|
| `surtgis-core` | 0.15.4 | **Lectura**: `read_vector()` → `FeatureCollection` | GeoJSON, GPKG (feature `geopackage`), Shapefile, GeoParquet. Geometrías `geo-types` 0.7. Ya es dependencia. |
| `geo` | 0.33.1 | Clipping + simplificación | `BooleanOps::{intersection, clip}` (polígonos y líneas contra rect del tile), `Simplify` (Douglas-Peucker), `SimplifyVwPreserve` (topología). Comparte `geo-types` 0.7 con surtgis → interop directa. |
| `mvt` | 0.13.0 | **Encoding** MVT (protobuf) | Encoder dedicado y pequeño (Tile/Layer/Feature + comandos de geometría). Mantenido (2026-04). |
| `geozero` | 0.15.1 | Alternativa de encoding | Más pesado (conversiones multi-formato); útil si después queremos FlatGeobuf. Para v0.2 basta `mvt`. |
| `flatgeobuf` | 6.0.1 | (v0.3+) input streaming | Fuera de alcance v0.2. |

Compresión gzip de tiles: `flate2` (ya es dependencia, GzEncoder).

**Elección**: `mvt` para encoding (directo, sin codegen protobuf propio),
`geo` para geometría. geozero queda como plan B si `mvt` se queda corto
con multi-geometrías.

## 3. Qué hace tippecanoe vs. qué replicamos

| Capacidad tippecanoe | v0.2 geotiles |
|---|---|
| Tiling por zoom + clipping con buffer | ✅ Sí (núcleo) |
| Simplificación por zoom (tolerancia ∝ resolución) | ✅ Sí (`geo::Simplify`, ε = k·res(z)) |
| Cuantización a extent 4096 + dedupe de vértices colapsados | ✅ Sí |
| Filtrado por zoom de geometrías degeneradas (polígono < 1 px²) | ✅ Sí (barato y de alto impacto) |
| Drop de features por densidad/tasa (`--drop-rate`, gamma) | ❌ No — heurísticas complejas; v0.2 emite advertencia si tile > 500 KB |
| Coalescing / clustering / attribute joins | ❌ No |
| Límite duro de tamaño de tile con re-simplificación iterativa | ❌ No (solo advertencia) |
| `minzoom/maxzoom` automáticos por densidad | ⚠️ Parcial: maxzoom por extensión del dataset; el usuario puede fijar ambos |

Para los casos de uso del ecosistema (capas temáticas chilenas:
cuencas, red hídrica, límites, puntos de muestreo — miles a cientos de
miles de features, no OSM planet), el drop heurístico no es necesario.

## 4. Arquitectura propuesta

```
crates/core/src/
├── vector.rs        VectorSource: surtgis read_vector → reproyección
│                    4326→3857 analítica (mercator.rs ya la tiene),
│                    índice espacial simple por bbox de feature
├── mvt.rs           Por tile: clip (bbox+buffer 64/4096) → simplify →
│                    cuantizar a extent 4096 → encode (crate mvt) → gzip
└── (reuso)          TileRange/TileCoord, TileSink, MbtilesSink, XyzSink
```

Cambios a infraestructura existente (menores):
- `PyramidMetadata.format`: ya existe; agregar `"pbf"` y campo
  `vector_layers` (JSON) — MBTiles 1.3 **exige** la fila `json` con
  `vector_layers` para tilesets vectoriales (MapLibre lo lee).
- `XyzSink`: extensión de archivo parametrizada (`.png` / `.pbf`).
- CLI: subcomando `geotiles vector in.geojson -o out.mbtiles
  [--layer nombre] [--min-zoom] [--max-zoom] [--simplify k]`.

Paralelización: mismo patrón rayon + writer thread. Cada tile es
independiente (clip de las features cuyo bbox intersecta).

## 5. Alcance v0.2

**In**: GeoJSON y GPKG de entrada (4326/3857); Point/MultiPoint/
LineString/MultiLineString/Polygon/MultiPolygon; una capa por corrida;
propiedades string/numéricas/bool; MBTiles pbf+gzip y árbol XYZ .pbf;
buffer estándar 64/4096; simplificación DP por zoom.

**Out (documentado)**: feature dropping heurístico, múltiples capas por
tileset (v0.3), Shapefile/GeoParquet de entrada (los lectores existen;
se habilitan si cuesta cero), reproyección general, GeometryCollection.

## 6. Validación (sin tippecanoe instalado)

1. **GDAL MVT driver** (verificado disponible, rw): `ogrinfo` sobre
   nuestros `.pbf` y MBTiles → round-trip de geometrías y atributos.
2. **Referencia cruzada**: `ogr2ogr -f MVT` genera un tileset del mismo
   GeoJSON → comparar conteos de features por tile y estructura.
3. **Visor**: extender `examples/viewer.html` con una fuente `vector`
   (MapLibre la soporta nativo).
4. (Opcional, si se instala tippecanoe desde fuente) comparación visual
   y de tamaños.

## 7. Esfuerzo y riesgos

| Etapa | Contenido | Estimado |
|---|---|---|
| 1 | vector.rs + mvt.rs con Point/LineString/Polygon simples, MBTiles pbf, CLI, validación GDAL | ~1 sesión |
| 2 | Multi-geometrías, filtrado de degenerados, simplificación afinada, visor, tests de borde | ~1 sesión |

**Riesgos**:
- *Winding order/validez post-clip*: `BooleanOps` de geo puede emitir
  anillos que MVT exige en orden CW/CCW específico → normalizar al
  cuantizar (área con signo). Riesgo medio, conocido y testeable.
- *Versión geo 0.33 vs geo 0.29 de surtgis*: comparten geo-types 0.7;
  cargo unifica. Riesgo bajo (verificar en lockfile al agregar).
- *Tiles gordos sin drop heurístico*: mitigado con advertencia + docs
  (usar tippecanoe para datasets masivos; geotiles apunta a capas
  temáticas).

## 8. Decisiones a confirmar antes de implementar

1. ¿GO con este alcance? (alternativa: diferir MVT y priorizar release
   0.1.0 a crates.io + WebP, que son más cortos)
2. ¿Una capa por corrida es suficiente para los frontends SvelteKit
   actuales, o se necesita multi-capa desde el día 1?
3. ¿GPKG entra en v0.2 o basta GeoJSON? (GPKG = habilitar feature
   `geopackage` de surtgis-core; costo casi nulo)
