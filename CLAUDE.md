# geotiles — Teselado y map tiles geoespaciales (Rust, "tippecanoe lite")

> **Estado:** v0.2 — raster (XYZ/MBTiles/COG/RGB) + vector tiles MVT. Creado 2026-06-10.
> Familia de motores Rust del autor: SurtGIS, Hydroflux, Smelt, Anvil, Cantus, Criterium.
> Doc madre: `~/proyectos/ideas-motores-rust.md` (idea L1 — habilitador, menos "paper").

## Qué es
Motor para generar teselas web: vector tiles (MVT) y raster tiles (XYZ), más
escritura COG/MBTiles. Habilitador transversal para tus frontends.

## El gap que llena
SurtGIS lee COG y hace STAC, pero no **publica** datos para web. El campo es
**tippecanoe** (C++), gdal2tiles (lento), mbutil. Infra reutilizable para todas
tus webs SvelteKit.

## Alcance MVP (v0.1) — COMPLETO
- [x] Raster → teselas XYZ (PNG) con remuestreo (nearest/bilinear) y pirámide.
- [x] Escritura COG (writer TIFF propio: Float32, deflate, overviews 2×;
      pasa el validador oficial de GDAL). Complementa el lector COG de SurtGIS.
- [x] Empaquetado MBTiles (SQLite, rusqlite bundled).
- [x] Promedio de área en zooms overview (sin tiles vacíos en zooms bajos).
- [x] RGB(A): lector multibanda, tiles true-color, COG byte.

## v0.2 — COMPLETO
- [x] Vector tiles MVT desde GeoJSON/GPKG; clip + simplificación por zoom.
- [ ] WebP como formato alternativo de tile.

## Arquitectura tentativa
- `geotiles-core`: pirámides, codecs de tile, MVT encoder.
- Targets: native (Rayon) + CLI principal; servidor de tiles ligero opcional.
- Reusa I/O raster/vector de SurtGIS.

## Validación
Comparar salidas contra **tippecanoe**/gdal2tiles (visual + estructura MBTiles).

## Venue objetivo
**SoftwareX** o **JOSS** (si se acota bien). Valor principal: infra, no paper.

## Conexiones con tu ecosistema
- **SurtGIS**: cierra el ciclo (analizar → publicar a web).
- Tus frontends SvelteKit (Criterium, territorio-digital, dashboards).

## Estado de implementación (2026-06-10)
- Workspace: `crates/core` (geotiles-core) + `crates/cli` (bin `geotiles`).
- Reutiliza `surtgis-core` (Raster, GeoTIFF nativo) y `surtgis-colormap`
  (16 esquemas, PNG) por path dependency.
- CLI: `geotiles raster in.tif -o out.mbtiles|dir/` y `geotiles info`.
- 22 unit tests + doctest; clippy limpio; validado con DEM real
  (dem_filled.tif reproyectado a 4326: 62 tiles z0–z13 en 56 ms).
- Visor de prueba: `examples/viewer.html` (MapLibre + ?tiles=dir).

## Limitaciones conocidas v0.1
- Solo entradas EPSG:4326/3857 (sin motor de reproyección; usar gdalwarp).
- Stretch RGB con un solo --range global para todas las bandas.
- Lector multibanda decodifica la imagen completa en RAM (igual que el
  lector nativo de surtgis); streaming queda para v0.2.

## Notas de implementación RGB(A) (2026-06-11)
- io.rs: lector multibanda propio sobre el crate tiff — el lector nativo
  de surtgis-core NO soporta multibanda (su parámetro band es no-op y RGB
  falla por dimensiones). Candidato a contribución upstream a surtgis.
- RasterSource ahora contiene 1/3/4 bandas co-registradas;
  sample_band/sample_area_band por banda.
- pyramid.rs: enum Shader interno (Colormap | Rgb); RGB stretch lineal a
  0-255, nodata→transparente, banda 4 = alpha.
- cog.rs: write_pyramid compartido parametrizado por SampleSpec;
  write_cog_rgb produce byte RGB(A) interleaved con ExtraSamples=2.

## Notas de implementación MVT (2026-06-13)
- vector.rs: VectorSource multi-capa (CLI llena una; push_layer lista para
  multi). Lector GeoJSON propio sobre crate `geojson` (el de surtgis-core
  no soporta LineString); GPKG vía surtgis read_gpkg. Reproyección 4326→3857.
- mvt.rs: por tile clip (geo BooleanOps) → simplify DP por zoom → cuantizar
  a extent 4096 → encode (crate `mvt`) → gzip. Paralelo rayon+writer.
- **Bug clave resuelto**: el encoder `mvt` 0.13 NO resetea su punto previo
  en complete_geom; partes consecutivas que comparten endpoint cuantizado se
  mis-codifican (LineTo sin MoveTo) → GDAL rechaza el tile. Fix:
  avoid_shared_endpoints (revierte líneas, rota anillos) + idioma canónico
  (complete_geom entre partes, encode cierra la última).
- Versiones: geo 0.29 y rusqlite 0.39 alineadas con surtgis-core (conflicto
  de i_overlay/i_float y libsqlite3-sys si difieren los major).
- Validación: 170 tiles aceptados por driver MVT de GDAL; feature-count por
  tile idéntico a ogr2ogr -f MVT (12/12 en z10); render MapLibre OK.

## CLI multi-capa MVT (2026-06-13)
- Subcomando `vector` toma N inputs, cada uno `[nombre=]ruta[#tabla_gpkg]`;
  cada input es una capa MVT. Nombre por defecto = stem. `#tabla` permite
  varias capas desde un mismo GPKG.
- VectorSource.push_file (refactor read_file compartido) apila capas desde
  archivo; rechaza nombres de capa duplicados (id MVT debe ser único).
- Validado: 3 capas (cuencas/red/estaciones) separadas en metadata
  vector_layers y dentro del tile; 170 tiles OK por GDAL.

## Próximos pasos al retomar
1. WebP como formato de tile alternativo.
2. Considerar release 0.2.0 a crates.io (raster + RGB(A) + MVT completos).
3. Contribuir upstream a surtgis-core: lectura multibanda (band no-op) y
   GeoJSON con geometrías de línea.
