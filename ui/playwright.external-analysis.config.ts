import { defineConfig } from '@playwright/test';

const backendURL = requiredEnvironment('KEEPPEEK_CONFORMANCE_BACKEND_URL');

export default defineConfig({
	testDir: './e2e',
	testMatch: 'external-analysis-conformance.e2e.ts',
	fullyParallel: false,
	workers: 1,
	retries: process.env.CI ? 1 : 0,
	reporter: [['list']],
	outputDir: 'test-results/external-analysis-conformance',
	timeout: 90_000,
	expect: { timeout: 15_000 },
	use: { baseURL: backendURL, headless: true, screenshot: 'off', trace: 'off' }
});

function requiredEnvironment(name: string): string {
	const value = process.env[name];
	if (!value) throw new Error(`${name} is required`);
	return value;
}
