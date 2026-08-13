import { defineConfig } from 'vitest/config';
import { playwright } from '@vitest/browser-playwright';
import tailwindcss from '@tailwindcss/vite';
import { sveltekit } from '@sveltejs/kit/vite';

const apiTarget = process.env.KEEPPEEK_API_TARGET ?? 'http://localhost:3000';
const isCI = Boolean(process.env.CI);

export default defineConfig({
	plugins: [tailwindcss(), sveltekit()],
	server: {
		proxy: {
			'/health': apiTarget,
			'/ready': apiTarget,
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
					exclude: ['src/lib/server/**']
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
