import { defineConfig } from '@playwright/test';

const port = process.env.KEEPPEEK_E2E_FRONTEND_PORT ?? '4174';
const baseURL = `http://127.0.0.1:${port}`;

export default defineConfig({
	testDir: './qa',
	testMatch: 'alpha-audit.spec.ts',
	fullyParallel: false,
	workers: 1,
	retries: 0,
	outputDir: 'test-results/alpha-audit',
	reporter: [['list'], ['json', { outputFile: 'test-results/alpha-audit.json' }]],
	use: { baseURL, headless: true },
	webServer: {
		command: `bun run dev -- --host 127.0.0.1 --port ${port}`,
		url: baseURL,
		reuseExistingServer: false
	}
});
