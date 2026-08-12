# Demo: analizar → publicar → servir (SurtGIS → geotiles → MapLibre)

El ciclo completo que geotiles habilita: **SurtGIS analiza** un raster,
**geotiles lo publica** como PMTiles (un archivo único por tileset), y
**MapLibre lo sirve** en el navegador con HTTP Range requests desde
cualquier hosting estático — sin servidor de tiles, sin costo de
infraestructura, escala solo.

```
SurtGIS (análisis)      geotiles (publicación)        Static host (servicio)
                                                                 │
 DEM ──→ slope °        slope_deg.tif ──→ slope.pmtiles ────────┼──┐
 DEM ──→ TPI            tpi.tif ──→ tpi.pmtiles ────────────────┼──┤ MapLibre
 DEM ──→ (relief)       dem.tif ──→ relief.pmtiles ─────────────┼──┤ (range
 GeoJSON ─────────────→ hidrografia.pmtiles ────────────────────┼──┘  requests)
```

## Qué muestra

- **Relief (DEM)**: el raster original con colormap `terrain`.
- **Slope °** y **TPI**: derivados de terreno calculados por SurtGIS
  (`surtgis terrain slope|tpi`), publicados como PMTiles raster.
- **Hidrografía**: capa vectorial MVT (cuencas + ríos + estaciones) en un
  PMTiles vector. Los ríos son **sintéticos realistas** — curvas tipo
  meandro confinadas al área del DEM, generadas por `gen_hydro.py` (el
  GeoJSON original con 6 puntos rectos por río se veía como líneas rectas
  cruzando el mapa).

Las 4 capas se renderizan en MapLibre leyendo los archivos `.pmtiles` vía
el protocolo `pmtiles://`, que hace requests `Range: bytes=...` al hosting.

## Requisitos

- `surtgis` en PATH (`~/.cargo/bin/surtgis`).
- `geotiles` compilado (`cargo build` → `target/debug/geotiles`, o en PATH).
- Un DEM en EPSG:4326 en `data/dem.tif`.
- La hidrografía se genera automáticamente (`gen_hydro.py`) si no hay
  `data/hidrografia.geojson`.

## Regenerar los datos

```bash
./generate-demo.sh
```

Produce `data/*.pmtiles` a partir del DEM. Los datos generados están
gitignoreados; el script los recrea.

## Servir y ver

PMTiles necesita un host que responda HTTP Range. El `range_server.py`
incluido es un mini servidor estático con soporte Range (el mismo
comportamiento de S3 / Cloudflare R2 / GitHub Pages):

```bash
python3 range_server.py 8734      # desde examples/demo-pmtiles/
# abrir http://localhost:8734/demo.html
```

En producción basta subir los `.pmtiles` a un bucket estático con range
requests habilitado y apuntar el `pmtiles://` al bucket.
