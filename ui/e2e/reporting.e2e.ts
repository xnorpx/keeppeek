import { expect, test, type Page } from '@playwright/test';
import { readFile } from 'node:fs/promises';
import { gunzipSync } from 'node:zlib';
import { mockControlPeer } from './fixtures/control-peer';
import { reportingRequestHandler, reportingService } from './reporting-service';

type MockOptions = {
	generations?: string[];
	configured?: boolean;
	failGrantStatus?: number;
	dropFirstFinalize?: boolean;
};

async function installReportingFixture(page: Page, options: MockOptions = {}) {
	const generations = [...(options.generations ?? ['recorder-gen-1', 'recorder-gen-1'])];
	const serviceOrigin = 'https://reporting.invalid';
	const privateCanaries = ['private-canary-secret', '192.168.2.44', 'camera-secret'];
	const service = reportingService(() => Date.now(), privateCanaries);
	let snapshotCalls = 0;
	let droppedFinalize = false;
	const serviceRequests: string[] = [];
	const recorderReportingRequests: string[] = [];
	const auditPayloads: Array<Record<string, unknown>> = [];

	await page.route('**/logs/snapshot', async (route) => {
		const call = snapshotCalls++;
		const generation = generations[Math.min(call, generations.length - 1)];
		const entries =
			call === 0
				? [
						{
							sequence: 1,
							timestamp_ms: Date.UTC(2026, 7, 12, 12, 0, 0),
							level: 'info',
							target: 'keeppeek::server',
							message: 'server-before-window-marker',
							fields: {}
						}
					]
				: [
						{
							sequence: 1,
							timestamp_ms: Date.UTC(2026, 7, 12, 12, 0, 0),
							level: 'info',
							target: 'keeppeek::server',
							message: 'server-before-window-marker',
							fields: {}
						},
						{
							sequence: 2,
							timestamp_ms: Date.UTC(2026, 7, 12, 12, 0, 1),
							level: 'warn',
							target: 'keeppeek::server',
							message: 'server-inside-window-marker token=camera-secret 192.168.2.44',
							fields: {}
						}
					];
		await route.fulfill({
			contentType: 'application/json',
			headers: { 'Cache-Control': 'no-store' },
			body: JSON.stringify({
				entries,
				oldest_sequence: 1,
				newest_sequence: entries.at(-1)?.sequence ?? 1,
				truncated: false,
				generation,
				stats: {
					entry_count: entries.length,
					byte_count: 512,
					evicted_entries: 0,
					max_entries: 10_000,
					max_bytes: 8_388_608,
					active_streams: 0,
					max_streams: 8
				}
			})
		});
	});

	await page.route('**/metrics', async (route) => {
		await route.fulfill({
			contentType: 'text/plain',
			body: 'keeppeek_server_info{host="keeppeek-private.local"} 1\n'
		});
	});

	await page.route(
		(url) => url.pathname === '/logs',
		async (route) => {
			await route.fulfill({ contentType: 'text/event-stream', body: '' });
		}
	);

	await page.route('**/reporting/status', async (route) => {
		recorderReportingRequests.push('GET /reporting/status');
		await route.fulfill({
			contentType: 'application/json',
			headers: { 'Cache-Control': 'no-store' },
			body: JSON.stringify(
				options.configured
					? { configured: true, origin: serviceOrigin, retention_days: 14 }
					: { configured: false }
			)
		});
	});

	await page.route('**/reporting/grants', async (route) => {
		recorderReportingRequests.push('POST /reporting/grants');
		if (options.failGrantStatus) {
			await route.fulfill({
				status: options.failGrantStatus,
				contentType: 'application/json',
				body: JSON.stringify({ code: options.failGrantStatus, message: 'service unavailable' })
			});
			return;
		}
		const payload = route.request().postDataJSON() as {
			idempotencyKey: string;
			manifest: unknown;
		};
		const created = service.create(
			service.recorderKey,
			payload.idempotencyKey,
			payload.manifest,
			14
		);
		await route.fulfill({
			contentType: 'application/json',
			headers: { 'Cache-Control': 'no-store' },
			body: JSON.stringify(created)
		});
	});

	await page.route('**/reporting/audit', async (route) => {
		recorderReportingRequests.push('POST /reporting/audit');
		auditPayloads.push(route.request().postDataJSON() as Record<string, unknown>);
		await route.fulfill({ status: 204, headers: { 'Cache-Control': 'no-store' } });
	});

	await page.route(`${serviceOrigin}/**`, async (route) => {
		const request = route.request();
		const url = new URL(request.url());
		serviceRequests.push(`${request.method()} ${url.pathname}`);
		if (options.dropFirstFinalize && url.pathname.endsWith('/finalize') && !droppedFinalize) {
			droppedFinalize = true;
			const handler = reportingRequestHandler(service, new URL(page.url()).origin);
			const headers = new Headers(request.headers());
			await handler(new Request(request.url(), { method: 'POST', headers }));
			await route.abort('failed');
			return;
		}
		const handler = reportingRequestHandler(service, new URL(page.url()).origin);
		const headers = new Headers(request.headers());
		const body = request.postDataBuffer();
		const init: RequestInit = { method: request.method(), headers };
		if (body && request.method() !== 'GET' && request.method() !== 'HEAD') {
			init.body = new Uint8Array(body);
		}
		const response = await handler(new Request(request.url(), init));
		let responseBody = Buffer.from(await response.arrayBuffer());
		if (
			url.pathname.endsWith('/finalize') ||
			(request.method() === 'GET' && /^\/v1\/reports\/[^/]+$/.test(url.pathname))
		) {
			const parsed = JSON.parse(responseBody.toString('utf8')) as {
				state: string;
				issueUrl?: string;
			};
			if (parsed.state === 'complete' && parsed.issueUrl) {
				parsed.issueUrl = 'https://github.com/xnorpx/keeppeek/issues/4242';
				responseBody = Buffer.from(JSON.stringify(parsed));
			}
		}
		await route.fulfill({
			status: response.status,
			headers: Object.fromEntries(response.headers.entries()),
			body: responseBody
		});
	});

	await mockControlPeer(page, {
		accessRole: 'administrator',
		runtimeConfiguration: {
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

	return { service, serviceRequests, recorderReportingRequests, auditPayloads };
}

async function fillReportFields(page: Page) {
	const dialog = page.getByRole('dialog');
	await dialog.getByLabel('Issue title').fill('Playback stalled on clip');
	await dialog.getByLabel('Problem description').fill('Clip stopped at 192.168.2.44.');
	await dialog.getByLabel('Steps to reproduce').fill('Open Keep and press play.');
	await dialog.getByLabel('Expected behavior').fill('Playback advances smoothly.');
	await dialog.getByLabel('Actual behavior').fill('Playback stays paused.');
}

test('makes zero reporting requests when idle across settings and logs', async ({ page }) => {
	const { serviceRequests, recorderReportingRequests } = await installReportingFixture(page, {
		configured: true
	});
	await page.goto('/settings');
	await page.getByRole('link', { name: 'View logs' }).click();
	await expect(page).toHaveURL(/\/settings\/logs$/);
	expect(recorderReportingRequests).toEqual([]);
	expect(serviceRequests).toEqual([]);
});

test('submits one confirmed report with same-generation server logs and retries idempotently', async ({
	page
}) => {
	const { service, serviceRequests, auditPayloads } = await installReportingFixture(page, {
		configured: true,
		generations: ['recorder-gen-same', 'recorder-gen-same'],
		dropFirstFinalize: true
	});
	await page.goto('/settings');
	await page.getByRole('button', { name: 'Report a bug' }).click();
	const dialog = page.getByRole('dialog');
	await dialog.getByRole('button', { name: 'Collect logs only' }).click();
	await page.evaluate(() => console.info('browser-inside-window-marker'));
	await page
		.getByRole('complementary', { name: 'Bug report in progress' })
		.getByRole('button', { name: 'Stop', exact: true })
		.click();
	await expect(dialog).toHaveAccessibleName('Review bug report');
	await expect(dialog.getByText('1 browser and 1 server log entries')).toBeVisible();

	await fillReportFields(page);
	await dialog.getByRole('button', { name: 'Review submission' }).click();
	await expect(dialog.getByText('Clip stopped at [REDACTED_IP].', { exact: true })).toBeVisible();
	expect(serviceRequests).toEqual([]);

	await dialog.getByRole('button', { name: 'Confirm and submit' }).click();
	await expect(dialog.getByRole('button', { name: 'Retry submission' })).toBeVisible();
	await dialog.getByRole('button', { name: 'Retry submission' }).click();
	await expect(dialog.getByRole('link', { name: 'Open the created GitHub issue' })).toHaveAttribute(
		'href',
		'https://github.com/xnorpx/keeppeek/issues/4242'
	);

	expect(service.createdIssues()).toHaveLength(1);
	expect(service.createdIssues()[0]).not.toContain('192.168.2.44');
	expect(service.createdIssues()[0]).not.toContain('camera-secret');
	const auditText = JSON.stringify(auditPayloads);
	expect(auditPayloads.map((entry) => entry.event)).toContain('start');
	expect(auditPayloads.map((entry) => entry.event)).toContain('complete');
	expect(auditText).not.toContain('Playback stalled on clip');
	expect(auditText).not.toContain('192.168.2.44');
});

test('discloses recorder restart across log generation change and keeps local downloads when remote fails', async ({
	page
}) => {
	const { auditPayloads } = await installReportingFixture(page, {
		configured: true,
		generations: ['recorder-gen-1', 'recorder-gen-2'],
		failGrantStatus: 502
	});
	await page.goto('/settings');
	await page.getByRole('button', { name: 'Report a bug' }).click();
	const dialog = page.getByRole('dialog');
	await dialog.getByRole('button', { name: 'Collect logs only' }).click();
	await page.evaluate(() => console.info('restart-window-browser-marker'));
	await page
		.getByRole('complementary', { name: 'Bug report in progress' })
		.getByRole('button', { name: 'Stop', exact: true })
		.click();
	await expect(dialog).toHaveAccessibleName('Review bug report');
	await expect(dialog.getByText('The recorder restarted during the report.')).toBeVisible();
	await expect(dialog.getByText('1 browser and 0 server log entries')).toBeVisible();

	await fillReportFields(page);
	await dialog.getByRole('button', { name: 'Review submission' }).click();
	await dialog.getByRole('button', { name: 'Confirm and submit' }).click();
	await expect(dialog.getByRole('button', { name: 'Retry submission' })).toBeVisible();

	const downloadPromise = page.waitForEvent('download');
	await dialog.getByRole('button', { name: 'Download report diagnostics' }).click();
	const downloadedPath = await (await downloadPromise).path();
	if (!downloadedPath) throw new Error('Expected local fallback diagnostics package');
	const bundle = gunzipSync(await readFile(downloadedPath)).toString('utf8');
	expect(bundle).toContain('server-log-generation-changed');
	expect(bundle).toContain('restart-window-browser-marker');
	expect(bundle).not.toContain('server-inside-window-marker');
	expect(auditPayloads.map((entry) => entry.event)).toContain('failed');
	expect(auditPayloads.map((entry) => entry.event)).toContain('local_download');
});

test('falls back to logs-only on display permission denial without calling getUserMedia', async ({
	page
}) => {
	await page.addInitScript(() => {
		const win = window as unknown as {
			__getUserMediaCalled?: boolean;
			__displayConstraints?: unknown;
		};
		win.__getUserMediaCalled = false;
		Object.defineProperty(navigator, 'mediaDevices', {
			configurable: true,
			value: {
				getDisplayMedia: async (constraints: unknown) => {
					win.__displayConstraints = constraints;
					throw new DOMException('Permission denied', 'NotAllowedError');
				},
				getUserMedia: async () => {
					win.__getUserMediaCalled = true;
					throw new Error('getUserMedia must never be called');
				}
			}
		});
	});
	await installReportingFixture(page, { configured: false });
	await page.goto('/settings');
	await page.getByRole('button', { name: 'Report a bug' }).click();
	await page.getByRole('dialog').getByRole('button', { name: 'Record screen' }).click();
	const indicator = page.getByRole('complementary', { name: 'Bug report in progress' });
	await expect(indicator).toContainText('Screen unavailable. Collecting logs.');
	const captureCheck = await page.evaluate(() => {
		const win = window as unknown as {
			__getUserMediaCalled?: boolean;
			__displayConstraints?: unknown;
		};
		return {
			getUserMediaCalled: win.__getUserMediaCalled,
			displayConstraints: win.__displayConstraints
		};
	});
	expect(captureCheck.getUserMediaCalled).toBe(false);
	expect(captureCheck.displayConstraints).toEqual({ video: true, audio: false });
	await indicator.getByRole('button', { name: 'Stop', exact: true }).click();
	await expect(page.getByRole('dialog')).toHaveAccessibleName('Review bug report');
});

test('degrades honestly when CompressionStream and MediaRecorder are unavailable', async ({
	page
}) => {
	await page.addInitScript(() => {
		Object.defineProperty(window, 'MediaRecorder', { configurable: true, value: undefined });
	});
	await installReportingFixture(page, { configured: false });
	await page.goto('/settings');
	const reportButton = page.getByRole('button', { name: 'Report a bug' });
	await expect(reportButton).toBeVisible();
	await page.evaluate(() => {
		Object.defineProperty(window, 'CompressionStream', { configurable: true, value: undefined });
	});
	await reportButton.click();
	const dialog = page.getByRole('dialog');
	await expect(dialog.getByRole('button', { name: 'Record screen' })).toBeDisabled();
	await dialog.getByRole('button', { name: 'Collect logs only' }).click();
	await page
		.getByRole('complementary', { name: 'Bug report in progress' })
		.getByRole('button', { name: 'Stop', exact: true })
		.click();
	await expect(dialog).toHaveAccessibleName('Review bug report');
	await expect(dialog.getByText('The diagnostics package could not be prepared.')).toBeVisible();
	await expect(dialog.getByRole('button', { name: 'Download report diagnostics' })).toHaveCount(0);
});
