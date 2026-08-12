"""Genera un GeoJSON de hidrografía realista (meandros) para el área del DEM.

Ríos con muchos vértices y curvatura suave (tipo meandro), confinados al
bbox del DEM, más unas cuencas pequeñas y estaciones. Reemplaza el GeoJSON
sintético original (6 puntos rectos por río) que se veía como 'rayas'.
"""
import json, math, random, sys

rng = random.Random(42)

LON0, LON1 = -71.554, -71.247   # bbox del DEM
LAT0, LAT1 = -32.796, -32.639

def meandro(x0, y0, x1, y1, n=48, amp=0.0012, wavelength=None):
    """Curva tipo río: interpolación lineal + senos de varias frecuencias."""
    if wavelength is None:
        wavelength = max(0.008, math.dist((x0, y0), (x1, y1)) / 6)
    pts = []
    for i in range(n + 1):
        t = i / n
        x = x0 + (x1 - x0) * t
        y = y0 + (y1 - y0) * t
        # perpendicular
        dx, dy = x1 - x0, y1 - y0
        L = math.hypot(dx, dy) or 1.0
        px, py = -dy / L, dx / L
        wobble = (math.sin(t * 2 * math.pi * 2) * 0.6
                  + math.sin(t * 2 * math.pi * 5 + 1.3) * 0.3
                  + rng.uniform(-0.15, 0.15))
        x += px * amp * wobble
        y += py * amp * wobble
        # meandros de mayor escala a mitad de camino
        bend = math.sin(t * math.pi) * amp * 3.5 * (1 if t < 0.5 else -1) * 0.3
        x += px * bend
        y += py * bend
        pts.append([round(x, 6), round(y, 6)])
    return pts

# ríos: de cabeceras (bordes altos) hacia un drenaje común al sur-este
rios = [
    (("maipo_norte",), (-71.50, -32.65), (-71.40, -32.72)),  # tributario
    (("maipo_este",), (-71.30, -32.66), (-71.38, -32.72)),
    (("maipo_centro",), (-71.42, -32.64), (-71.36, -32.73)),
    (("maipo_oeste",), (-71.53, -32.70), (-71.44, -32.74)),
    (("maipo_principal",), (-71.44, -32.72), (-71.34, -32.75)),
    (("maipo_desague",), (-71.34, -32.75), (-71.27, -32.78)),
]
# conectar en un arbol: tributarios -> principal -> desague
features = []
for i, (names, (x0, y0), (x1, y1)) in enumerate(rios):
    coords = meandro(x0, y0, x1, y1)
    features.append({
        "type": "Feature", "properties": {"name": names[0], "orden": (i % 3) + 1},
        "geometry": {"type": "LineString", "coordinates": coords},
    })

# cuencas: polígonos pequeños anidados cerca de los ríos (no todo el mapa)
def poligono(cx, cy, r, n=20):
    ring = []
    for i in range(n + 1):
        a = 2 * math.pi * i / n
        rr = r * (1 + 0.25 * math.sin(3 * a) + rng.uniform(-0.08, 0.08))
        ring.append([round(cx + rr * math.cos(a), 6), round(cy + rr * math.sin(a), 6)])
    return ring

for i, (cx, cy, r) in enumerate([
    (-71.45, -32.70, 0.012), (-71.36, -32.72, 0.010), (-71.49, -32.74, 0.009),
    (-71.31, -32.75, 0.011), (-71.42, -32.67, 0.008), (-71.38, -32.77, 0.010),
    (-71.51, -32.77, 0.007), (-71.29, -32.70, 0.009),
]):
    features.append({
        "type": "Feature", "properties": {"name": f"cuenca_{i}", "area_km2": round(r * 111 * r * 111 * 3.14, 1)},
        "geometry": {"type": "Polygon", "coordinates": [poligono(cx, cy, r)]},
    })

# estaciones a lo largo de los ríos
for i in range(12):
    lon = rng.uniform(LON0 + 0.01, LON1 - 0.01)
    lat = rng.uniform(LAT0 + 0.01, LAT1 - 0.01)
    features.append({
        "type": "Feature", "properties": {"estacion": f"E{i:02d}"},
        "geometry": {"type": "Point", "coordinates": [round(lon, 6), round(lat, 6)]},
    })

out = {"type": "FeatureCollection", "features": features}
path = sys.argv[1] if len(sys.argv) > 1 else "hidrografia.geojson"
with open(path, "w") as f:
    json.dump(out, f)
print(f"escrito {path}: {len(features)} features "
      f"({len(rios)} ríos meandro, 8 cuencas, 12 estaciones)")
