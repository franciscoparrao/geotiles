# geotiles — Teselado y map tiles geoespaciales (Rust, "tippecanoe lite")

> **Estado:** v0.1 en desarrollo — pipeline raster funcionando (XYZ + MBTiles). Creado 2026-06-10.
> Familia de motores Rust del autor: SurtGIS, Hydroflux, Smelt, Anvil, Cantus, Criterium.
> Doc madre: `~/proyectos/ideas-motores-rust.md` (idea L1 — habilitador, menos "paper").

## Qué es
Motor para generar teselas web: vector tiles (MVT) y raster tiles (XYZ), más
escritura COG/MBTiles. Habilitador transversal para tus frontends.

## El gap que llena
SurtGIS lee COG y hace STAC, pero no **publica** datos para web. El campo es
**tippecanoe** (C++), gdal2tiles (lento), mbutil. Infra reutilizable para todas
tus webs SvelteKit.

## Alcance MVP (v0.1)
- [x] Raster → teselas XYZ (PNG) con remuestreo (nearest/bilinear) y pirámide.
- [ ] Escritura COG (complementa el lector COG de SurtGIS).
- [x] Empaquetado MBTiles (SQLite, rusqlite bundled).
- [ ] (v0.2) Vector tiles MVT desde GeoJSON/GPKG; simplificación por zoom.
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
- Point-sampling en centros de píxel: en zooms muy bajos un raster chico
  puede caer entre muestras → tiles vacíos (z0/z2 en el smoke test).
  Pendiente: overviews por promedio de área.
- Una banda por corrida; RGB(A) pendiente.

## Próximos pasos al retomar
1. Overviews por promedio de área (downsampling desde el zoom base, no
   point-sampling por zoom) — corrige tiles vacíos en zooms bajos.
2. Escritura COG (cierra el ítem MVP restante).
3. Soporte RGB(A) de 3–4 bandas (write_geotiff_multiband ya existe en SurtGIS).
4. Evaluar MVT para v0.2.
