import { defineConfig } from '@playwright/test';

const port = process.env.KEEPPEEK_TIMELINE_PERF_PORT ?? '4175';
const baseURL = `http://127.0.0.1:${port}`;

export default defineConfig({
	testDir: './performance',
	testMatch: '**/*.performance.ts',
	fullyParallel: false,
	workers: 1,
	retries: 0,
	reporter: 'line',
	outputDir: 'test-results/timeline-performance',
	timeout: 180_000,
	use: {
		baseURL,
		headless: true,
		trace: 'off',
		colorScheme: 'dark',
		reducedMotion: 'reduce'
	},
	webServer: {
		command: `bunx vite build --config visual-harness/vite.local.config.ts && bunx vite preview --config visual-harness/vite.local.config.ts --host 127.0.0.1 --port ${port}`,
		url: `${baseURL}/local-preview.html`,
		reuseExistingServer: false,
		timeout: 180_000
	}
});
