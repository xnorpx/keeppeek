import { expect, test, type Page } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';

const config = {
	host: '0.0.0.0',
	port: 8080,
	storage: {
		medium_term_path: '/mnt/keeppeek',
		long_term_path: '/mnt/keeppeek',
		recording_catalog_path: '/mnt/keeppeek/recordings.db',
		event_thumbnail_path: '/mnt/keeppeek/.event-thumbnails',
		event_thumbnail_max_mb: 512,
		short_term_secs: 10,
		medium_term_secs: 3600,
		flush_interval_secs: 5,
		write_buffer_bytes: 1_048_576,
		long_term_max_gb: 0
	},
	camera_count: 0,
	recording_estimate: {
		estimated_bitrate_bps: 0,
		bytes_per_day: 0,
		known_streams: 0,
		unknown_streams: 0,
		estimated_retention_days: null
	}
};

const health = {
	version: '0.4.1-test',
	system: {
		disks: [
			{
				name: 'recordings',
				kind: 'SSD',
				file_system: 'apfs',
				mount_point: '/mnt/keeppeek',
				total_bytes: 8_000_000_000_000,
				available_bytes: 7_900_000_000_000,
				used_bytes: 100_000_000_000,
				removable: false,
				stores_recordings: true
			}
		]
	}
};

async function mockFirstRun(page: Page) {
	const writes: string[] = [];
	const controls = await mockControlPeer(page, { runtimeConfiguration: config, health });
	page.on('request', (request) => {
		const pathname = new URL(request.url()).pathname;
		if (request.method() !== 'GET' && pathname !== '/create' && pathname !== '/delete') {
			writes.push(`${request.method()} ${request.url()}`);
		}
	});
	return { controls, writes };
}

test('Board 21 verifies storage, names empty states, and opens camera onboarding', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const { controls, writes } = await mockFirstRun(page);

	await page.goto('/setup');

	await expect(page.locator('[data-shell-context]')).toContainText('First run');
	await expect(page.getByRole('heading', { name: 'Start with evidence' })).toBeVisible();
	await expect(page.locator('[data-first-run-panel]')).toBeVisible();
	await expect(page.getByText('/mnt/keeppeek', { exact: true })).toBeVisible();
	await expect(page.getByText('7.9 TB FREE · CAPACITY OBSERVED')).toBeVisible();
	await expect(page.locator('[data-storage-write-status]')).toHaveAttribute(
		'data-storage-write-status',
		'verified'
	);
	await expect(page.getByText('WRITE PROOF VERIFIED')).toBeVisible();
	await expect(page.getByText('Write, flush, rename, and cleanup succeeded.')).toBeVisible();
	await expect(page.getByText('DETECTED FROM THIS BROWSER')).toBeVisible();
	await expect(page.getByText(/Server update required · keeppeek\.identity\.v1/)).toBeVisible();
	const startRecorder = page.getByRole('button', { name: 'Continue to camera setup' });
	await expect(startRecorder).toBeEnabled();
	await expect(startRecorder).toBeInViewport();
	await expect(page.getByRole('heading', { name: 'No cameras yet' })).toBeVisible();
	await expect(page.locator('[data-first-run-empty-states] [data-empty-state]')).toHaveCount(3);
	await expect(page.getByText('NO FOOTAGE YET')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Registry unavailable' })).toBeDisabled();
	expect(controls.storageProbePaths).toEqual(['/mnt/keeppeek']);
	await expect(page.locator('[data-first-run-panel]')).not.toContainText(
		'DETECTED FROM THIS MACHINE'
	);
	await expect(page.locator('[data-first-run-empty-states]')).not.toContainText(
		'0 EVENT SOURCES REGISTERED'
	);
	expect(writes).toEqual([]);

	await page.getByRole('link', { name: 'Change recording storage' }).click();
	await expect(page).toHaveURL(/\/settings\?edit=storage#storage$/);
	await expect(page.getByRole('heading', { name: 'Change recording storage' })).toBeVisible();
	await expect(page.getByLabel('Folder path')).toHaveValue('/mnt/keeppeek');
	await expect(page.getByText('Confirm unbounded storage before continuing.')).toBeVisible();
	expect(writes).toEqual([]);

	await page.goto('/setup');
	await page.getByRole('button', { name: 'Continue to camera setup' }).click();
	await expect(page).toHaveURL(/\/cameras\/new$/);
	await expect(page.getByRole('heading', { name: 'Add camera' })).toBeVisible();
	expect(writes).toEqual([]);

	await page.goto('/setup');
	await page.getByRole('link', { name: 'Enter an address' }).click();
	await expect(page).toHaveURL(/\/cameras\/new#manual-camera$/);
	await expect(page.getByRole('heading', { name: 'Connect directly' })).toBeVisible();
	expect(writes).toEqual([]);
});
