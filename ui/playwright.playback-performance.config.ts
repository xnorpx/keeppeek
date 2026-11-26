import { defineConfig } from '@playwright/test';

export default defineConfig({
	testDir: './performance',
	testMatch: '**/playback.performance.ts',
	fullyParallel: false,
	workers: 1,
	retries: 0,
	reporter: 'line',
	outputDir: 'test-results/playback-performance',
	timeout: 180_000,
	use: {
		headless: true,
		trace: 'off',
		colorScheme: 'dark',
		reducedMotion: 'reduce',
		viewport: { width: 1440, height: 900 }
	}
});
