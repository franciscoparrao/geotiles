#!/usr/bin/env bash
# Generate the demo tilesets this example serves.
#
# Produces MBTiles AND PMTiles under data/ from sources kept in the repo:
#   - relief.mbtiles / relief.pmtiles   : terrain relief from a DEM
#   - hidrografia.mbtiles / hidrografia.pmtiles : thematic vector layer (MVT)
#
# The MBTiles are served by the /tiles/<layer> endpoints; the PMTiles by the
# /pmtiles/<layer> proxy (range requests). Open the app with ?backend=pmtiles
# (default) or ?backend=mbtiles to pick.
#
# Requires the `geotiles` binary on PATH (cargo install geotiles), plus
# `gdalwarp` if the DEM still needs reprojecting to EPSG:4326.
set -euo pipefail
cd "$(dirname "$0")/data"

GEOTILES="${GEOTILES:-geotiles}"

# --- Raster: DEM -> terrain relief tiles -------------------------------
# Ships a small DEM already in EPSG:4326. If you bring your own in another
# CRS, reproject first: gdalwarp -t_srs EPSG:4326 in.tif dem.tif
if [[ ! -f dem.tif ]]; then
  echo "data/dem.tif not found."
  echo "Drop a small EPSG:4326 GeoTIFF there (see README), then re-run."
  exit 1
fi
echo "==> relief.mbtiles + relief.pmtiles"
"$GEOTILES" raster dem.tif -o relief.mbtiles --scheme terrain --max-zoom 13
"$GEOTILES" raster dem.tif -o relief.pmtiles --scheme terrain --max-zoom 13

# --- Vector: thematic layer -> MVT tiles -------------------------------
echo "==> hidrografia.mbtiles + hidrografia.pmtiles"
"$GEOTILES" vector hidrografia.geojson -o hidrografia.mbtiles \
  --name hidrografia --max-zoom 14
"$GEOTILES" vector hidrografia.geojson -o hidrografia.pmtiles \
  --name hidrografia --max-zoom 14

echo "done: $(ls -1 *.mbtiles *.pmtiles)"
