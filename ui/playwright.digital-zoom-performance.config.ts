import { defineConfig } from '@playwright/test';
import baseConfig from './playwright.config';

const servers = Array.isArray(baseConfig.webServer) ? baseConfig.webServer : [];

export default defineConfig({
	...baseConfig,
	testMatch: '**/digital-zoom.performance.ts',
	workers: 1,
	webServer: servers.map((server, index) => ({
		...server,
		reuseExistingServer: index === 1 && process.env.KEEPPEEK_ZOOM_BASELINE === '1'
	}))
});
