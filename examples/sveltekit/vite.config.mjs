import { sveltekit } from '@sveltejs/kit/vite';

/** @type {import('vite').UserConfig} */
export default {
  plugins: [sveltekit()],
  // better-sqlite3 is a native module; keep it external to the SSR bundle.
  ssr: { external: ['better-sqlite3'] }
};

