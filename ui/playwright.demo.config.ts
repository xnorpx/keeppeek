import { defineConfig } from '@playwright/test';

const backendPort = process.env.KEEPPEEK_E2E_BACKEND_PORT ?? '4317';
const backendURL = `http://127.0.0.1:${backendPort}`;
const frontendPort = process.env.KEEPPEEK_E2E_FRONTEND_PORT ?? '4174';
const baseURL = `http://127.0.0.1:${frontendPort}`;
const environment = Object.fromEntries(
	Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined)
);

export default defineConfig({
	testDir: './demo',
	testMatch: '**/camera-lifecycle.demo.ts',
	fullyParallel: false,
	workers: 1,
	timeout: 180_000,
	reporter: 'list',
	outputDir: 'test-results/demo-playwright',
	expect: { timeout: 30_000 },
	use: {
		baseURL,
		headless: true,
		colorScheme: 'dark',
		trace: 'retain-on-failure'
	},
	webServer: [
		{
			command: 'bun scripts/start-logging-e2e-server.ts',
			url: `${backendURL}/metrics`,
			env: { ...environment, KEEPPEEK_E2E_EMPTY_FLEET: '1' },
			reuseExistingServer: false,
			timeout: 180_000
		},
		{
			command: `bun run dev -- --host 127.0.0.1 --port ${frontendPort}`,
			url: baseURL,
			env: { ...environment, KEEPPEEK_API_TARGET: backendURL },
			reuseExistingServer: false
		}
	]
});
