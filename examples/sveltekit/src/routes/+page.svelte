<script>
  import { onMount } from 'svelte';
  import maplibregl from 'maplibre-gl';
  import 'maplibre-gl/dist/maplibre-gl.css';

  let mapEl;
  let status = $state('cargando tilesets…');

  onMount(async () => {
    // Both tilesets are served from MBTiles by this app's own endpoints.
    const [relief, hidro] = await Promise.all([
      fetch('/tiles/relief/metadata').then((r) => r.json()),
      fetch('/tiles/hidrografia/metadata').then((r) => r.json())
    ]);
    const srcLayer = hidro.vector_layers?.[0]?.id ?? 'hidrografia';
    const [w, s, e, n] = relief.bounds;
    status = `relief z${relief.minzoom}–${relief.maxzoom} · ${hidro.name} (vector)`;

    const map = new maplibregl.Map({
      container: mapEl,
      style: {
        version: 8,
        sources: {
          relief: {
            type: 'raster',
            tiles: [`${location.origin}/tiles/relief/{z}/{x}/{y}`],
            tileSize: 256,
            minzoom: relief.minzoom,
            maxzoom: relief.maxzoom,
            bounds: relief.bounds
          },
          hidro: {
            type: 'vector',
            tiles: [`${location.origin}/tiles/hidrografia/{z}/{x}/{y}`],
            minzoom: hidro.minzoom,
            maxzoom: hidro.maxzoom,
            bounds: hidro.bounds
          }
        },
        layers: [
          { id: 'bg', type: 'background', paint: { 'background-color': '#dfe7ef' } },
          { id: 'relief', type: 'raster', source: 'relief' },
          {
            id: 'hidro-fill',
            type: 'fill',
            source: 'hidro',
            'source-layer': srcLayer,
            filter: ['==', ['geometry-type'], 'Polygon'],
            paint: { 'fill-color': '#2563eb', 'fill-opacity': 0.25, 'fill-outline-color': '#1e3a8a' }
          },
          {
            id: 'hidro-line',
            type: 'line',
            source: 'hidro',
            'source-layer': srcLayer,
            filter: ['==', ['geometry-type'], 'LineString'],
            paint: { 'line-color': '#0e7490', 'line-width': 2 }
          },
          {
            id: 'hidro-point',
            type: 'circle',
            source: 'hidro',
            'source-layer': srcLayer,
            filter: ['==', ['geometry-type'], 'Point'],
            paint: {
              'circle-radius': 5,
              'circle-color': '#e11d48',
              'circle-stroke-color': '#fff',
              'circle-stroke-width': 1.5
            }
          }
        ]
      },
      bounds: [[w, s], [e, n]],
      fitBoundsOptions: { padding: 50 }
    });
    map.addControl(new maplibregl.NavigationControl());
  });
</script>

<svelte:head><title>geotiles · SvelteKit</title></svelte:head>

<main>
  <header>
    <h1>geotiles → SvelteKit</h1>
    <p>{status}</p>
  </header>
  <div class="map" bind:this={mapEl}></div>
</main>

<style>
  :global(body) { margin: 0; font-family: system-ui, sans-serif; }
  main { display: flex; flex-direction: column; height: 100vh; }
  header { padding: 0.6rem 1rem; background: #0f172a; color: #e2e8f0; }
  header h1 { margin: 0; font-size: 1rem; font-weight: 600; }
  header p { margin: 0.15rem 0 0; font-size: 0.8rem; color: #94a3b8; }
  .map { flex: 1; }
</style>
