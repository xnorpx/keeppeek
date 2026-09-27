import { defineConfig } from '@playwright/test';

export default defineConfig({
	testDir: '..',
	testMatch: 'external-authentication.e2e.ts',
	workers: 1,
	forbidOnly: Boolean(process.env.CI),
	timeout: 90_000,
	expect: { timeout: 15_000 },
	use: { headless: true, ignoreHTTPSErrors: true, trace: 'off', screenshot: 'only-on-failure' }
});
