import { realpathSync } from 'node:fs';
import { resolve } from 'node:path';
import { defineConfig } from 'vitest/config';
import { playwright } from '@vitest/browser-playwright';
import tailwindcss from '@tailwindcss/vite';
import { sveltekit } from '@sveltejs/kit/vite';

const apiTarget = process.env.KEEPPEEK_API_TARGET ?? 'http://localhost:3000';
const isCI = Boolean(process.env.CI);
const uiRoot = resolve('.');
const fontSourceRoots = [
	realpathSync(resolve(uiRoot, 'node_modules/@fontsource/archivo')),
	realpathSync(resolve(uiRoot, 'node_modules/@fontsource/ibm-plex-mono'))
];

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()],
	optimizeDeps: {
		include: [
			'@lucide/svelte/icons/arrow-up',
			'@lucide/svelte/icons/check',
			'@lucide/svelte/icons/check-check',
			'@lucide/svelte/icons/chevron-up',
			'@lucide/svelte/icons/copy',
			'@lucide/svelte/icons/download',
			'@lucide/svelte/icons/eye',
			'@lucide/svelte/icons/eye-off',
			'@lucide/svelte/icons/flask-conical',
			'@lucide/svelte/icons/inbox',
			'@lucide/svelte/icons/key-round',
			'@lucide/svelte/icons/loader-circle',
			'@lucide/svelte/icons/log-out',
			'@lucide/svelte/icons/plus',
			'@lucide/svelte/icons/refresh-cw',
			'@lucide/svelte/icons/rotate-cw',
			'@lucide/svelte/icons/shield-off',
			'@lucide/svelte/icons/trash-2'
		]
	},
	server: {
		fs: {
			allow: [uiRoot, ...fontSourceRoots]
		},
		proxy: {
			'/create': apiTarget,
			'/delete': apiTarget,
			'/logs': apiTarget,
			'/metrics': apiTarget,
			'/api': apiTarget
		}
	},
	test: {
		expect: { requireAssertions: true },
		reporters: isCI
			? [
					'default',
					'github-actions',
					['junit', { outputFile: 'test-results/vitest.junit.xml' }],
					['json', { outputFile: 'test-results/vitest.json' }]
				]
			: ['default'],
		coverage: {
			provider: 'v8',
			reporter: ['text-summary', 'json-summary', 'lcov', 'html'],
			reportsDirectory: 'coverage'
		},
		projects: [
			{
				extends: './vite.config.ts',
				test: {
					name: 'client',
					browser: {
						enabled: true,
						provider: playwright(),
						instances: [{ browser: 'chromium', headless: true }]
					},
					include: ['src/**/*.svelte.{test,spec}.{js,ts}'],
					exclude: ['src/lib/server/**', 'src/**/*.story.svelte.spec.ts']
				}
			},
			{
				extends: './vite.config.ts',
				test: {
					name: 'visual',
					browser: {
						enabled: true,
						provider: playwright(),
						instances: [
							{
								browser: 'chromium',
								headless: true,
								viewport: { width: 1440, height: 900 }
							}
						]
					},
					include: ['src/**/*.story.svelte.spec.ts']
				}
			},

			{
				extends: './vite.config.ts',
				test: {
					name: 'server',
					environment: 'node',
					include: ['src/**/*.{test,spec}.{js,ts}'],
					exclude: ['src/**/*.svelte.{test,spec}.{js,ts}']
				}
			}
		]
	}
});
