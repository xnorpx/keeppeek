import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';

const config = {
	host: '0.0.0.0',
	port: 3000,
	camera_count: 2,
	storage: {
		medium_term_path: '/recordings',
		long_term_path: '/recordings',
		recording_catalog_path: '/recordings/recordings.db',
		event_thumbnail_path: '/recordings/.event-thumbnails',
		event_thumbnail_max_mb: 1024,
		short_term_secs: 120,
		medium_term_secs: 1800,
		flush_interval_secs: 60,
		write_buffer_bytes: 1_048_576,
		long_term_max_gb: 2048
	},
	recording_estimate: {
		estimated_bitrate_bps: 8_000_000,
		bytes_per_day: 86_400_000_000,
		known_streams: 2,
		unknown_streams: 0,
		estimated_retention_days: 25.4
	}
};

test('Board 14 shows catalog event evidence without inventing a publisher registry', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const writes: string[] = [];
	page.on('request', (request) => {
		const pathname = new URL(request.url()).pathname;
		if (request.method() !== 'GET' && pathname !== '/create' && pathname !== '/delete') {
			writes.push(`${request.method()} ${request.url()}`);
		}
	});
	await mockControlPeer(page, {
		runtimeConfiguration: config,
		health: {
			system: { disks: [] },
			storage: {
				long_term_max_bytes: 2_199_023_255_552,
				catalog_bytes: 8_388_608,
				catalog: {
					recording_files: 1_000,
					finalized_files: 990,
					active_files: 10,
					fragments: 50_000,
					fragment_bytes: 1_800_000_000_000,
					events: 1_402,
					open_events: 2,
					event_thumbnails: 350
				}
			}
		}
	});

	await page.goto('/settings#event-sources');

	const section = page.getByRole('region', { name: 'Event sources' });
	await expect(section).toBeInViewport();
	expect(
		await section.evaluate((element) => element.getBoundingClientRect().width)
	).toBeGreaterThan(1200);
	await expect(section.getByText('1,402', { exact: true })).toBeVisible();
	await expect(section.getByText('Total events', { exact: true })).toBeVisible();
	await expect(section).toContainText('Catalog counts are all-time aggregates');
	await expect(section).toContainText('camera');
	await expect(section).toContainText('keeppeek');
	await expect(section).toContainText('SOURCE REGISTRY UNAVAILABLE');
	await expect(section).toContainText('NOT PUBLISHER IDENTITIES');
	await expect(section.getByRole('button', { name: 'Register a source' })).toBeDisabled();
	await expect(section.getByRole('button', { name: 'Manage source' })).toBeDisabled();
	await expect(section.getByRole('button', { name: 'Rotate token' })).toBeDisabled();
	await expect(section.getByRole('link', { name: 'Browse stored event evidence' })).toHaveAttribute(
		'href',
		'/events'
	);

	for (const field of ['source', 'confidence', 'bbox', 'zone', 'thumbnail_url']) {
		await expect(section.getByText(field, { exact: true })).toBeVisible();
	}
	for (const field of ['source_id', 'revision', 'text', 'payload', 'attachments[]']) {
		await expect(section.getByText(field, { exact: true })).toBeVisible();
	}
	await expect(section).toContainText('Protocol types only');
	await expect(section.getByText('object-detect', { exact: true })).toHaveCount(0);
	await expect(section.getByText('doorbell-bridge', { exact: true })).toHaveCount(0);
	await expect(section.getByText('1,402 EVENTS INGESTED TODAY', { exact: true })).toHaveCount(0);
	await expect(section.getByText(/kp_[a-z0-9]+/)).toHaveCount(0);
	expect(writes).toEqual([]);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});
