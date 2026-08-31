import { defineConfig } from '@playwright/test';

const backendURL = 'http://127.0.0.1:4318';
const port = process.env.KEEPPEEK_PEEK_PERF_PORT ?? '4175';
const baseURL = `http://127.0.0.1:${port}`;
const browserChannel = process.env.KEEPPEEK_PEEK_BROWSER_CHANNEL;
const environment = Object.fromEntries(
	Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined)
);

export default defineConfig({
	testDir: './performance',
	testMatch: '**/peek-transitions.performance.ts',
	fullyParallel: false,
	workers: 1,
	retries: 0,
	reporter: 'line',
	outputDir: 'test-results/peek-performance',
	timeout: 300_000,
	expect: { timeout: 30_000 },
	use: {
		baseURL,
		headless: true,
		colorScheme: 'dark',
		trace: 'retain-on-failure',
		...(browserChannel ? { channel: browserChannel } : {})
	},
	webServer: [
		{
			command: 'bun scripts/start-nine-camera-demo-server.ts',
			url: `${backendURL}/metrics`,
			env: { ...environment, RUST_LOG: process.env.RUST_LOG ?? 'warn' },
			reuseExistingServer: false,
			timeout: 300_000
		},
		{
			command: `bun run dev -- --host 127.0.0.1 --port ${port}`,
			url: baseURL,
			env: { ...environment, KEEPPEEK_API_TARGET: backendURL },
			reuseExistingServer: false,
			timeout: 120_000
		}
	]
});
