import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

const buildDir = process.env.KEEPPEEK_UI_BUILD_DIR ?? 'build';
// Cargo builds must not replace the generated modules used by a running dev server.
const outDir = process.env.KEEPPEEK_UI_BUILD_DIR ? `${buildDir}-svelte-kit` : '.svelte-kit';

/** @type {import('@sveltejs/kit').Config} */
const config = {
	preprocess: vitePreprocess({ script: true }),
	kit: {
		outDir,
		adapter: adapter({
			pages: buildDir,
			assets: buildDir,
			fallback: 'index.html',
			strict: false
		})
	}
};

export default config;
