import { expect, test, type Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';

const buffer = {
	entry_count: 1,
	byte_count: 256,
	evicted_entries: 0,
	max_entries: 10_000,
	max_bytes: 8_388_608,
	active_streams: 1,
	max_streams: 8
};

const serverEntry = {
	sequence: 1,
	timestamp_ms: Date.UTC(2026, 7, 12, 12, 0, 0),
	level: 'info',
	target: 'keeppeek::server',
	message: 'server snapshot ready',
	fields: {}
};

async function installLoggingMocks(page: Page): Promise<void> {
	let activeFilter = 'info,keeppeek=debug';
	await page.addInitScript(() => {
		class TestEventSource extends EventTarget {
			onopen: ((event: Event) => void) | null = null;
			onerror: ((event: Event) => void) | null = null;
			readonly readyState = 1;
			readonly url: string;
			readonly withCredentials = false;

			constructor(url: string | URL) {
				super();
				this.url = String(url);
				setTimeout(() => {
					this.onopen?.(new Event('open'));
					this.dispatchEvent(
						new MessageEvent('log', {
							data: JSON.stringify({
								sequence: 2,
								timestamp_ms: Date.UTC(2026, 7, 12, 12, 0, 1),
								level: 'warn',
								target: 'str0m',
								message: 'live packet queue warning',
								fields: { depth: 512 }
							})
						})
					);
				}, 0);
			}

			close(): void {}
		}
		Object.defineProperty(window, 'EventSource', { configurable: true, value: TestEventSource });
	});

	await page.route('**/api/config', async (route) => {
		await route.fulfill({
			json: {
				host: '0.0.0.0',
				port: 3000,
				camera_count: 0,
				storage: {
					medium_term_path: '/recordings',
					long_term_path: '/recordings',
					recording_catalog_path: '/recordings/recordings.db',
					event_thumbnail_path: '/recordings/.event-thumbnails',
					event_thumbnail_max_mb: 1024,
					short_term_secs: 120,
					medium_term_secs: 1800,
					flush_interval_secs: 60,
					write_buffer_bytes: 8192,
					long_term_max_gb: 0
				},
				recording_estimate: {
					estimated_bitrate_bps: 0,
					bytes_per_day: 0,
					known_streams: 0,
					unknown_streams: 0,
					estimated_retention_days: null
				}
			}
		});
	});
	await page.route('**/api/settings/cameras', async (route) => {
		await route.fulfill({ json: [] });
	});
	await page.route('**/api/settings/logging', async (route) => {
		if (route.request().method() === 'PUT') {
			const update = route.request().postDataJSON() as { filter: string };
			if (update.filter.includes('verbose')) {
				await route.fulfill({ status: 400, json: { error: 'invalid log filter' } });
				return;
			}
			activeFilter = update.filter;
		}
		await route.fulfill({
			json: {
				active_filter: activeFilter,
				default_filter: 'info,keeppeek=debug',
				filter_error: null,
				version: '0.1.0',
				buffer
			}
		});
	});
	await page.route('**/api/logs?*', async (route) => {
		await route.fulfill({
			json: {
				entries: [serverEntry],
				oldest_sequence: 1,
				newest_sequence: 1,
				truncated: false,
				stats: buffer
			}
		});
	});
}

test('views, filters, captures, persists, clears, and exports logs', async ({ page }) => {
	await installLoggingMocks(page);
	await page.goto('/settings');
	await page.getByRole('link', { name: 'View logs' }).click();

	await expect(page).toHaveURL(/\/settings\/logs$/);
	await expect(page.getByRole('heading', { name: 'Logs' })).toBeVisible();
	await expect(page.getByText('server snapshot ready')).toBeVisible();
	await expect(page.getByText('live packet queue warning')).toBeVisible();
	await expect(page.getByText('connected', { exact: true })).toBeVisible();

	await page.getByLabel('Server capture filter').fill('info,str0m=warn');
	await page.getByRole('button', { name: 'Save filter' }).click();
	await expect(page.getByText('Active: info,str0m=warn')).toBeVisible();

	await page.getByLabel('Server capture filter').fill('keeppeek=verbose');
	await page.getByRole('button', { name: 'Save filter' }).click();
	await expect(page.getByRole('alert')).toContainText('invalid log filter');
	await expect(page.getByText('Active: info,str0m=warn')).toBeVisible();

	await page.evaluate(() => {
		console.error('browser capture failed token=browser-secret');
		window.dispatchEvent(
			new ErrorEvent('error', {
				message: 'global browser failure',
				error: new Error('global browser failure')
			})
		);
		window.dispatchEvent(
			new PromiseRejectionEvent('unhandledrejection', {
				promise: Promise.resolve(),
				reason: new Error('rejected browser operation')
			})
		);
	});
	await page.getByRole('tab', { name: 'Browser / Svelte' }).click();
	await expect(page.getByText('browser capture failed token=[REDACTED]')).toBeVisible();
	await expect(page.getByText('global browser failure', { exact: true })).toBeVisible();
	await expect(page.getByText('browser.promise.unhandled', { exact: true })).toBeVisible();

	await page.reload();
	await page.getByRole('tab', { name: 'Browser / Svelte' }).click();
	await expect(page.getByText('browser capture failed token=[REDACTED]')).toBeVisible();

	const downloadPromise = page.waitForEvent('download');
	await page.getByRole('button', { name: 'Download bug report' }).click();
	const download = await downloadPromise;
	expect(download.suggestedFilename()).toMatch(/^keeppeek-bug-report-.*\.jsonl$/);
	const downloadPath = await download.path();
	expect(downloadPath).not.toBeNull();
	const contents = await readFile(downloadPath!, 'utf8');
	const records = contents
		.trim()
		.split('\n')
		.map((line) => JSON.parse(line));
	expect(records.map((record) => record.type)).toContain('server_log');
	expect(records.map((record) => record.type)).toContain('browser_log');
	expect(contents).toContain('server snapshot ready');
	expect(contents).toContain('browser capture failed token=[REDACTED]');
	expect(contents).not.toContain('browser-secret');

	await page.getByRole('button', { name: 'Clear log view' }).click();
	await expect(page.getByText('browser capture failed token=[REDACTED]')).toHaveCount(0);
});

test('keeps the log toolbar usable on a narrow viewport', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await installLoggingMocks(page);
	await page.goto('/settings/logs');

	await expect(page.getByRole('heading', { name: 'Logs' })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Pause' })).toBeInViewport();
	await expect(page.getByLabel('Text filter')).toBeInViewport();
	const viewportWidth = await page.evaluate(() => document.documentElement.scrollWidth);
	expect(viewportWidth).toBeLessThanOrEqual(390);
});
