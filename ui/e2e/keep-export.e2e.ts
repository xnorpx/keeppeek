import { readFile } from 'node:fs/promises';
import { expect, test } from '@playwright/test';
import type { Page } from '@playwright/test';
import type { MockControlPeerOptions } from './fixtures/control-peer';
import { keepModeDate, keepModeDayStartMs, mockKeepModes } from './fixtures/keep-modes';

const mediaExportCapability = 'keeppeek.media-export.v1';
const selectedStartMs = keepModeDayStartMs + 6 * 60 * 60_000 + 20 * 60_000;
const selectedEndMs = keepModeDayStartMs + 6 * 60 * 60_000 + 22 * 60_000;

function observeNoncanonicalControlRequests(page: Page): string[] {
	const requests: string[] = [];
	page.on('request', (request) => {
		const pathname = new URL(request.url()).pathname;
		if (request.method() !== 'GET' && pathname !== '/create' && pathname !== '/delete') {
			requests.push(`${request.method()} ${pathname}`);
		}
	});
	return requests;
}

async function openSupportedExport(page: Page, options: MockControlPeerOptions = {}) {
	const noncanonicalRequests = observeNoncanonicalControlRequests(page);
	const controls = await mockKeepModes(page, 10, {
		...options,
		capabilityIds: [mediaExportCapability]
	});
	await page.goto(`/keep?camera=front-door&stream=main&date=${keepModeDate}&mode=export`);
	return {
		controls,
		noncanonicalRequests,
		panel: page.locator('[data-keep-export]')
	};
}

async function selectTwoMinuteRange(page: Page): Promise<void> {
	const panel = page.locator('[data-keep-export]');
	const fromInput = panel.getByRole('textbox', { name: 'FROM', exact: true });
	const toInput = panel.getByRole('textbox', { name: 'TO', exact: true });
	await fromInput.fill('06:20:00');
	await fromInput.press('Tab');
	await toInput.fill('06:22:00');
	await toInput.press('Tab');
	await expect(panel).toHaveAttribute('data-export-start-ms', String(selectedStartMs));
	await expect(panel).toHaveAttribute('data-export-end-ms', String(selectedEndMs));
}

test('preserves an export range while job creation fails closed', async ({ page }) => {
	await mockKeepModes(page);
	const noncanonicalRequests = observeNoncanonicalControlRequests(page);
	await page.goto(`/keep?camera=front-door&stream=main&date=${keepModeDate}&mode=export`);

	await expect(page.getByRole('button', { name: 'Export', exact: true })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
	const panel = page.locator('[data-keep-export]');
	await expect(panel).toHaveAttribute(
		'data-export-start-ms',
		String(keepModeDayStartMs + 6 * 60 * 60_000 + 15 * 60_000)
	);
	await expect(page.getByText('120.0 MB', { exact: true })).toBeVisible();

	const fromInput = panel.getByRole('textbox', { name: 'FROM', exact: true });
	const toInput = panel.getByRole('textbox', { name: 'TO', exact: true });
	await fromInput.fill('06:20:00');
	await fromInput.press('Tab');
	await toInput.fill('06:22:00');
	await toInput.press('Tab');
	await expect(panel).toHaveAttribute(
		'data-export-start-ms',
		String(keepModeDayStartMs + 6 * 60 * 60_000 + 20 * 60_000)
	);
	await expect(page.getByText('120.0 MB', { exact: true })).toBeVisible();
	await page.getByRole('checkbox', { name: /burn in timestamp/i }).check();

	const gate = page
		.locator('[data-capability-gate][data-capability="keeppeek.media-export.v1"]')
		.filter({ hasText: 'Create export' });
	await expect(gate).toContainText('Server update required · keeppeek.media-export.v1');
	await expect(fromInput).toHaveValue('06:20:00');
	await expect(page.getByRole('checkbox', { name: /burn in timestamp/i })).toBeChecked();
	expect(noncanonicalRequests).toEqual([]);
});

test('shows Board 29 running progress and cancels without offering a partial file', async ({
	page
}) => {
	const { controls, noncanonicalRequests, panel } = await openSupportedExport(page, {
		exportCreateResults: [
			{
				status: 'running',
				progress: 0.62,
				bytesWritten: 74_000_000,
				estimatedBytes: 118_000_000
			}
		]
	});
	await selectTwoMinuteRange(page);
	await page.getByRole('button', { name: 'Create export' }).click();

	await expect(panel).toHaveAttribute('data-export-status', 'running');
	await expect(page.getByRole('progressbar', { name: 'Export progress' })).toHaveAttribute(
		'aria-valuenow',
		'62'
	);
	await expect(page.getByText('62% · 74 MB OF 118 MB')).toBeVisible();
	await page.getByRole('button', { name: 'Cancel', exact: true }).click();
	await expect(panel).toHaveAttribute('data-export-status', 'cancelled');
	await expect(page.getByText('No partial file was kept.')).toBeVisible();
	expect(controls.exportJobs.map((request) => request.action)).toContain('cancel');
	expect(noncanonicalRequests).toEqual([]);
});

test('polls a running export to ready and downloads checksum-verified WebRTC bytes', async ({
	page
}) => {
	const exportFile = Uint8Array.from([0, 0, 0, 8, 102, 116, 121, 112, 105, 115, 111, 109]);
	const { controls, noncanonicalRequests, panel } = await openSupportedExport(page, {
		exportFile,
		exportCreateResults: [
			{ status: 'running', progress: 0.4, bytesWritten: 4, estimatedBytes: exportFile.byteLength }
		],
		exportGetResults: [
			{
				status: 'ready',
				progress: 1,
				bytesWritten: exportFile.byteLength,
				estimatedBytes: exportFile.byteLength,
				alignedStartMs: selectedStartMs - 1_000,
				fileName: 'Front-Door_2026-08-18T06-20-00-000Z_to_2026-08-18T06-22-00-000Z.mp4'
			}
		]
	});
	await selectTwoMinuteRange(page);
	await page.getByRole('button', { name: 'Create export' }).click();
	await expect(panel).toHaveAttribute('data-export-status', 'running');
	await expect(panel).toHaveAttribute('data-export-status', 'ready');
	await expect(page.getByText('Your file is ready')).toBeVisible();
	await expect(page.locator('[data-export-checksum]')).toContainText('· · ·');
	await expect(page.getByText(/preceding keyframe/)).toBeVisible();

	const downloadPromise = page.waitForEvent('download');
	await page.getByRole('button', { name: 'Download', exact: true }).click();
	const download = await downloadPromise;
	expect(download.suggestedFilename()).toBe(
		'Front-Door_2026-08-18T06-20-00-000Z_to_2026-08-18T06-22-00-000Z.mp4'
	);
	const downloadPath = await download.path();
	expect(downloadPath).not.toBeNull();
	expect(new Uint8Array(await readFile(downloadPath!))).toEqual(exportFile);
	expect(controls.exportJobs.map((request) => request.action)).toEqual(
		expect.arrayContaining(['create', 'get', 'download'])
	);
	expect(noncanonicalRequests).toEqual([]);
});

test('names every missing range before exporting what exists', async ({ page }) => {
	const missingStartMs = keepModeDayStartMs + 6 * 60 * 60_000 + 21 * 60_000;
	const missingEndMs = missingStartMs + 30_000;
	const { controls, noncanonicalRequests, panel } = await openSupportedExport(page, {
		exportCreateResults: [
			{
				status: 'partial',
				estimatedBytes: 96_000_000,
				missingRanges: [{ startMs: missingStartMs, endMs: missingEndMs }]
			},
			{
				status: 'ready',
				bytesWritten: 96_000_000,
				estimatedBytes: 96_000_000
			}
		]
	});
	await selectTwoMinuteRange(page);
	await page.getByRole('button', { name: 'Create export' }).click();

	await expect(panel).toHaveAttribute('data-export-status', 'partial');
	await expect(page.getByText('1m 30s of 2m 0s you asked for')).toBeVisible();
	await expect(page.locator('[data-export-missing-range]')).toHaveText(
		'NOTHING WAS RECORDED 06:21:00 → 06:21:30'
	);
	await page.getByRole('button', { name: 'Export what exists' }).click();
	await expect(panel).toHaveAttribute('data-export-status', 'ready');
	const createRequests = controls.exportJobs.filter((request) => request.action === 'create');
	expect(createRequests).toHaveLength(2);
	expect(createRequests[1]).toMatchObject({ allowPartial: true });
	expect(noncanonicalRequests).toEqual([]);
});

test('trims a partial export to the named gap boundary and preserves the draft', async ({
	page
}) => {
	const missingStartMs = keepModeDayStartMs + 6 * 60 * 60_000 + 21 * 60_000;
	const missingEndMs = missingStartMs + 30_000;
	const { controls, noncanonicalRequests, panel } = await openSupportedExport(page, {
		exportCreateResults: [
			{
				status: 'partial',
				missingRanges: [{ startMs: missingStartMs, endMs: missingEndMs }]
			}
		]
	});
	await selectTwoMinuteRange(page);
	await page.getByRole('button', { name: 'Create export' }).click();
	await page.getByRole('button', { name: 'Trim to 06:21:30' }).click();

	await expect(panel).toHaveAttribute('data-export-status', 'draft');
	await expect(panel).toHaveAttribute('data-export-start-ms', String(missingEndMs));
	await expect(panel.getByRole('textbox', { name: 'FROM', exact: true })).toHaveValue('06:21:30');
	expect(controls.exportJobs.filter((request) => request.action === 'create')).toHaveLength(1);
	expect(noncanonicalRequests).toEqual([]);
});

test('keeps a failed export range and retries the same server job', async ({ page }) => {
	const { controls, noncanonicalRequests, panel } = await openSupportedExport(page, {
		exportCreateResults: [
			{
				status: 'failed',
				progress: 0.41,
				error: 'export.write: no space left on device (/mnt/keeppeek/tmp)',
				retryable: true
			}
		],
		exportRetryResult: { status: 'running', progress: 0, estimatedBytes: 200_000_000 }
	});
	await selectTwoMinuteRange(page);
	await page.getByRole('button', { name: 'Create export' }).click();

	await expect(panel).toHaveAttribute('data-export-status', 'failed');
	await expect(page.getByText('The disk filled while writing')).toBeVisible();
	await expect(page.getByText(/no space left on device/)).toBeVisible();
	await expect(page.getByRole('link', { name: 'Open storage' })).toHaveAttribute(
		'href',
		'/settings#storage'
	);
	await expect(panel).toHaveAttribute('data-export-start-ms', String(selectedStartMs));
	await page.getByRole('button', { name: 'Retry', exact: true }).click();
	await expect(panel).toHaveAttribute('data-export-status', 'running');
	await expect(panel).toHaveAttribute('data-export-start-ms', String(selectedStartMs));
	expect(controls.exportJobs.map((request) => request.action)).toContain('retry');
	expect(noncanonicalRequests).toEqual([]);
});

test('reports timestamp burn-in as an explicit no-reencode failure', async ({ page }) => {
	const { controls, noncanonicalRequests, panel } = await openSupportedExport(page);
	await selectTwoMinuteRange(page);
	await page.getByRole('checkbox', { name: /burn in timestamp/i }).check();
	await page.getByRole('button', { name: 'Create export' }).click();

	await expect(panel).toHaveAttribute('data-export-status', 'failed');
	await expect(page.getByText('Timestamp burn-in is unavailable')).toBeVisible();
	await expect(page.getByText(/requires a configured re-encoding worker/)).toBeVisible();
	expect(controls.exportJobs.find((request) => request.action === 'create')).toMatchObject({
		burnInTimestamp: true
	});
	expect(noncanonicalRequests).toEqual([]);
});

test('offers an existing exact export or an explicit fresh artifact', async ({
	page
}, testInfo) => {
	const { controls, noncanonicalRequests, panel } = await openSupportedExport(page, {
		exportJobs: [
			{
				jobId: 'matching-ready',
				status: 'ready',
				requestedStartMs: selectedStartMs,
				requestedEndMs: selectedEndMs
			}
		],
		exportCreateResults: [{ status: 'running', progress: 0.1 }]
	});
	await selectTwoMinuteRange(page);

	await expect(page.getByText('A matching export is already ready')).toBeVisible();
	await testInfo.attach('duplicate-ready-desktop.png', {
		body: await panel.screenshot(),
		contentType: 'image/png'
	});
	expect(controls.exportJobs.filter((request) => request.action === 'create')).toHaveLength(0);
	await page.getByRole('button', { name: 'Create fresh export' }).click();
	await expect(panel).toHaveAttribute('data-export-status', 'running');
	expect(controls.exportJobs.filter((request) => request.action === 'create')).toHaveLength(1);
	expect(noncanonicalRequests).toEqual([]);
});

test('shows an identical active export instead of starting duplicate work', async ({ page }) => {
	const initialStartMs = keepModeDayStartMs + 6 * 60 * 60_000 + 15 * 60_000;
	const { controls, noncanonicalRequests, panel } = await openSupportedExport(page, {
		exportJobs: [
			{
				jobId: 'matching-active',
				status: 'running',
				progress: 0.35,
				requestedStartMs: initialStartMs,
				requestedEndMs: initialStartMs + 2 * 60_000
			}
		]
	});

	await expect(panel).toHaveAttribute('data-export-status', 'running');
	await expect(page.locator('[data-export-job="matching-active"]')).toBeVisible();
	expect(controls.exportJobs.filter((request) => request.action === 'create')).toHaveLength(0);
	expect(noncanonicalRequests).toEqual([]);
});

test('reuses a matching ready server job without horizontal drift at the Paper mobile width', async ({
	page
}, testInfo) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const { noncanonicalRequests, panel } = await openSupportedExport(page, {
		exportJobs: [
			{
				jobId: 'restored-export',
				status: 'ready',
				requestedStartMs: keepModeDayStartMs + 6 * 60 * 60_000 + 15 * 60_000,
				requestedEndMs: keepModeDayStartMs + 6 * 60 * 60_000 + 17 * 60_000,
				bytesWritten: 118_000_000,
				estimatedBytes: 118_000_000,
				fileName: 'Front-Door_2026-08-18T06-11-48-000Z_to_2026-08-18T06-13-48-000Z.mp4'
			}
		]
	});
	await expect(page.getByText('A matching export is already ready')).toBeVisible();
	await testInfo.attach('duplicate-ready-mobile.png', {
		body: await panel.screenshot(),
		contentType: 'image/png'
	});
	await page.getByRole('button', { name: 'Use existing export' }).click();
	await expect(panel).toHaveAttribute('data-export-status', 'ready');
	await expect(page.getByText('Your file is ready')).toBeVisible();
	const bounds = await panel.boundingBox();
	expect(bounds).not.toBeNull();
	expect(bounds!.x).toBeGreaterThanOrEqual(0);
	expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(390);
	expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
	expect(noncanonicalRequests).toEqual([]);
});
