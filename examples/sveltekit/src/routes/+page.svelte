<script>
  import { onMount } from 'svelte';
  import { browser } from '$app/environment';
  import maplibregl from 'maplibre-gl';
  import 'maplibre-gl/dist/maplibre-gl.css';
  import { Protocol, PMTiles } from 'pmtiles';

  let mapEl;
  let status = $state('cargando tilesets…');

  onMount(async () => {
    if (!browser) return;
    const origin = window.location.origin;

    // PMTiles: un archivo único por tileset, leído por rangos a través
    // del protocolo pmtiles:// (registrado abajo). El endpoint
    // /pmtiles/<layer> hace de proxy con soporte Range.
    const protocol = new Protocol();
    maplibregl.addProtocol('pmtiles', protocol.tile);

    const header = async (layer) => {
      // getHeader usa range requests; el proxy /pmtiles/<layer> los sirve.
      const p = new PMTiles(`${origin}/pmtiles/${layer}`);
      const h = await p.getHeader();
      const meta = await p.getMetadata().catch(() => ({}));
      return { h, meta };
    };

    const relief = await header('relief');
    const hidro = await header('hidrografia');
    const w = relief.h.minLon, s = relief.h.minLat;
    const e = relief.h.maxLon, n = relief.h.maxLat;
    const srcLayer = hidro.meta?.vector_layers?.[0]?.id ?? 'hidrografia';
    status = `PMTiles · relief z${relief.h.minZoom}–${relief.h.maxZoom} · hidrografía (vector)`;

    const sources = {
      relief: {
          type: 'raster',
          tiles: [`pmtiles://${origin}/pmtiles/relief/{z}/{x}/{y}`],
          tileSize: 256
        },
        hidro: {
          type: 'vector',
          tiles: [`pmtiles://${origin}/pmtiles/hidrografia/{z}/{x}/{y}`]
        }
      };

    const map = new maplibregl.Map({
      container: mapEl,
      style: {
        version: 8,
        sources,
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
