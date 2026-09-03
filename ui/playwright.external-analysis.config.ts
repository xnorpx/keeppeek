import { defineConfig } from '@playwright/test';

const backendURL = requiredEnvironment('KEEPPEEK_CONFORMANCE_BACKEND_URL');

export default defineConfig({
	testDir: './e2e',
	testMatch: 'external-analysis-conformance.e2e.ts',
	fullyParallel: false,
	workers: 1,
	retries: 0,
	reporter: [['list']],
	outputDir: 'test-results/external-analysis-conformance',
	expect: { timeout: 15_000 },
	use: { baseURL: backendURL, headless: true }
});

function requiredEnvironment(name: string): string {
	const value = process.env[name];
	if (!value) throw new Error(`${name} is required`);
	return value;
}
