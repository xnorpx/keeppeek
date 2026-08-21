import { fileURLToPath, URL } from 'node:url';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	root: fileURLToPath(new URL('.', import.meta.url)),
	plugins: [tailwindcss(), svelte()],
	resolve: {
		alias: {
			$lib: fileURLToPath(new URL('../src/lib', import.meta.url)),
			'$app/paths': fileURLToPath(new URL('./stories/sveltekit-paths.ts', import.meta.url))
		}
	},
	server: {
		fs: { allow: [fileURLToPath(new URL('..', import.meta.url))] }
	}
});
