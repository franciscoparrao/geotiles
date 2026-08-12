#!/usr/bin/env bash
# Regenerate the SurtGIS → geotiles → PMTiles demo tilesets.
#
# The full pipeline this demo stands for:
#   1. SurtGIS ANALYSES  : terrain derivatives from a DEM (slope, TPI)
#   2. geotiles PUBLISHES: raster/vector → single-file .pmtiles archives
#   3. MapLibre SERVES   : the browser reads the archives via HTTP Range
#                          requests from any static host (no server).
#
# Requires:
#   - surtgis on PATH (~/.cargo/bin/surtgis)
#   - geotiles on PATH (cargo build -p geotiles)
#   - a DEM in EPSG:4326 at data/dem.tif (see README)
#   - hidrografia.geojson (a small vector layer) at data/
set -euo pipefail
cd "$(dirname "$0")"

SURTGIS="${SURTGIS:-surtgis}"
GEOTILES="${GEOTILES:-geotiles}"
mkdir -p data

if [[ ! -f data/dem.tif ]]; then
  echo "data/dem.tif not found. Drop an EPSG:4326 GeoTIFF there (see README)."
  exit 1
fi

echo "==> 1. SurtGIS analiza el DEM"
"$SURTGIS" terrain slope data/dem.tif data/slope_deg.tif --units degrees
"$SURTGIS" terrain tpi   data/dem.tif data/tpi.tif

echo "==> 1b. Hidrografía sintética realista (meandros confinados al DEM)"
if [[ ! -f data/hidrografia.geojson ]]; then
  python3 gen_hydro.py data/hidrografia.geojson
fi

echo "==> 2. geotiles publica PMTiles"
"$GEOTILES" raster data/dem.tif        -o data/relief.pmtiles    --scheme terrain  --max-zoom 12
"$GEOTILES" raster data/slope_deg.tif  -o data/slope.pmtiles     --scheme terrain  --range 0,90 --max-zoom 12
"$GEOTILES" raster data/tpi.tif        -o data/tpi.pmtiles       --scheme divergent --range=-0.5,0.5 --max-zoom 12
"$GEOTILES" vector "hidrografia=data/hidrografia.geojson" -o data/hidrografia.pmtiles --max-zoom 12

echo "==> listo:"
ls -la data/*.pmtiles

echo ""
echo "Para servir (range requests, como un bucket estático):"
echo "  python3 range_server.py 8734     # o cualquier static host con soporte Range"
echo "  abrir http://localhost:8734/demo.html"
