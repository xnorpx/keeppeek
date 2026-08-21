import { fileURLToPath, URL } from 'node:url';
import type { StorybookConfig } from '@storybook/svelte-vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import tailwindcss from '@tailwindcss/vite';
import { mergeConfig } from 'vite';

const productionRoot = fileURLToPath(new URL('../..', import.meta.url));

const config: StorybookConfig = {
	stories: ['../stories/**/*.stories.ts'],
	addons: ['@storybook/addon-essentials', '@storybook/addon-a11y'],
	framework: {
		name: '@storybook/svelte-vite',
		options: {}
	},
	async viteFinal(existingConfig) {
		return mergeConfig(existingConfig, {
			plugins: [tailwindcss(), svelte()],
			resolve: {
				alias: {
					$lib: fileURLToPath(new URL('../../src/lib', import.meta.url)),
					'$app/paths': fileURLToPath(new URL('../stories/sveltekit-paths.ts', import.meta.url))
				}
			},
			server: {
				fs: { allow: [productionRoot] }
			}
		});
	}
};

export default config;
