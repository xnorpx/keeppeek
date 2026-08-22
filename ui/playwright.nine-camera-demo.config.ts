import { defineConfig } from '@playwright/test';

const backendURL = 'http://127.0.0.1:4318';
const baseURL = 'http://127.0.0.1:4175';
const environment = Object.fromEntries(
	Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined)
);

export default defineConfig({
	testDir: './demo',
	testMatch: '**/nine-camera-live.demo.ts',
	fullyParallel: false,
	workers: 1,
	timeout: 300_000,
	reporter: 'list',
	outputDir: 'test-results/nine-camera-demo-playwright',
	expect: { timeout: 90_000 },
	use: {
		baseURL,
		headless: true,
		colorScheme: 'dark',
		trace: 'retain-on-failure'
	},
	webServer: [
		{
			command: 'bun scripts/start-nine-camera-demo-server.ts',
			url: `${backendURL}/metrics`,
			reuseExistingServer: false,
			timeout: 300_000
		},
		{
			command: 'bun run dev -- --host 127.0.0.1 --port 4175',
			url: baseURL,
			env: { ...environment, KEEPPEEK_API_TARGET: backendURL },
			reuseExistingServer: false
		}
	]
});
