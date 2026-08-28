import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

const buildDir = process.env.KEEPPEEK_UI_BUILD_DIR ?? 'build';
const outDir = process.env.KEEPPEEK_SVELTE_KIT_DIR ?? '.svelte-kit';

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
