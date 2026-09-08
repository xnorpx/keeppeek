import { defineConfig } from '@playwright/test';
import peekPerformance from './playwright.peek-performance.config';

export default defineConfig({
	...peekPerformance,
	testMatch: '**/peek-wall.performance.ts',
	use: { ...peekPerformance.use, viewport: { width: 1440, height: 900 } },
	outputDir: 'test-results/peek-wall-performance'
});
