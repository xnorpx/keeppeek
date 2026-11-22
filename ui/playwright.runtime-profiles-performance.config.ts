import { defineConfig } from '@playwright/test';

const port = process.env.KEEPPEEK_E2E_FRONTEND_PORT ?? '4174';
const baseURL = 'http://127.0.0.1:' + port;

export default defineConfig({
	testDir: './e2e',
	testMatch: '**/runtime-profiles.performance.ts',
	workers: 1,
	retries: 0,
	timeout: 15 * 60_000,
	outputDir: 'test-results/profile-performance',
	use: { baseURL },
	webServer: {
		command: 'bun run dev -- --host 127.0.0.1 --port ' + port + ' --strictPort',
		url: baseURL,
		reuseExistingServer: false
	}
});
