import { defineConfig } from '@playwright/test';

const port = process.env.KEEPPEEK_CARD_FRONTEND_PORT ?? '49563';
const baseURL = `http://127.0.0.1:${port}`;

export default defineConfig({
	testDir: './e2e',
	testMatch: '**/home-assistant.e2e.ts',
	fullyParallel: false,
	workers: 1,
	timeout: 60_000,
	use: { baseURL, headless: true, trace: 'retain-on-failure' },
	outputDir: 'test-results/home-assistant',
	reporter: 'list',
	webServer: {
		command: `bun run dev:home-assistant -- --host 127.0.0.1 --port ${port} --strictPort`,
		url: `${baseURL}/src/lib/home-assistant/config.ts`,
		reuseExistingServer: false
	}
});
