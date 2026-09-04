import { defineConfig } from '@playwright/test';

const backendPort = process.env.KEEPPEEK_E2E_BACKEND_PORT ?? '4317';
const backendURL = `http://127.0.0.1:${backendPort}`;
const frontendPort = process.env.KEEPPEEK_E2E_FRONTEND_PORT ?? '4174';
const baseURL = `http://127.0.0.1:${frontendPort}`;
const isCI = Boolean(process.env.CI);
const environment = Object.fromEntries(
	Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined)
);

export default defineConfig({
	testDir: './e2e',
	testMatch: '**/*.e2e.{ts,js}',
	testIgnore: '**/external-analysis-conformance.e2e.ts',
	fullyParallel: true,
	forbidOnly: Boolean(process.env.CI),
	retries: process.env.CI ? 2 : 0,
	reporter: isCI
		? [
				['github'],
				['junit', { outputFile: 'test-results/playwright.junit.xml' }],
				['json', { outputFile: 'test-results/playwright.json' }],
				['html', { outputFolder: 'playwright-report', open: 'never' }]
			]
		: 'list',
	outputDir: 'test-results/playwright',
	expect: {
		timeout: 10_000
	},
	use: {
		baseURL,
		headless: true,
		trace: 'on-first-retry'
	},
	webServer: [
		{
			command: 'bun scripts/start-logging-e2e-server.ts',
			url: `${backendURL}/metrics`,
			env: environment,
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
