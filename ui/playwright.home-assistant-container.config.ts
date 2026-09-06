import { defineConfig } from '@playwright/test';

export default defineConfig({
	testDir: './home-assistant-tests',
	testMatch: '**/*.spec.ts',
	fullyParallel: false,
	workers: 1,
	forbidOnly: true,
	retries: 0,
	timeout: 120_000,
	expect: { timeout: 15_000 },
	use: {
		headless: true,
		trace: 'off',
		screenshot: 'off',
		video: 'off',
		actionTimeout: 15_000,
		navigationTimeout: 30_000
	},
	outputDir: 'test-results/home-assistant-container',
	reporter: [['list'], ['junit', { outputFile: 'test-results/home-assistant-container.xml' }]]
});
