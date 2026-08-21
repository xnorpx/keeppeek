import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';

const config = {
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
		write_buffer_bytes: 1_048_576,
		long_term_max_gb: 2048
	},
	recording_estimate: {
		estimated_bitrate_bps: 0,
		bytes_per_day: 0,
		known_streams: 0,
		unknown_streams: 0,
		estimated_retention_days: null
	}
};

test('Board 17 shows integration egress contracts without inventing connection state', async ({
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
		health: { system: { disks: [] }, storage: { catalog: null } }
	});

	await page.goto('/settings#integrations');

	const section = page.getByRole('region', {
		name: 'Everything has an explicit egress boundary'
	});
	await expect(section).toBeInViewport();
	expect(
		await section.evaluate((element) => element.getBoundingClientRect().width)
	).toBeGreaterThan(1200);
	await expect(section).toContainText('NO THIRD-PARTY MEDIA RELAY');
	await expect(section.getByText('RUNTIME UNAVAILABLE')).toHaveCount(4);
	await expect(section.getByRole('button', { name: 'Configuration unavailable' })).toHaveCount(4);
	for (const button of await section
		.getByRole('button', { name: 'Configuration unavailable' })
		.all()) {
		await expect(button).toBeDisabled();
	}

	for (const integration of ['Home Assistant', 'MQTT event forwarder', 'Webhooks', 'Prometheus']) {
		await expect(section.getByText(integration, { exact: true })).toBeVisible();
	}
	await expect(section).toContainText('Direct browser card; Home Assistant is not a media proxy.');
	await expect(section).toContainText(
		'A configured dashboard browser would connect directly to KeepPeek.'
	);
	await expect(section).toContainText(
		'Events and selected attachments would leave through the forwarder.'
	);
	await expect(section).toContainText(
		'Configured event payloads would be pushed to each endpoint.'
	);
	await expect(section).toContainText(
		'No push. A remote collector would read metrics from KeepPeek.'
	);
	await expect(section.getByText('MISSING CONTRACTS')).toHaveCount(4);
	await expect(section).toContainText('direct card package');
	await expect(section).toContainText('forwarder binary');
	await expect(section).toContainText('durable retry queue');
	await expect(section).toContainText('scrape configuration UI');
	await expect(section).toContainText('scrape health evidence');
	await expect(section.getByText('/metrics', { exact: true })).toBeVisible();

	await expect(section.getByRole('link', { name: 'Health' })).toHaveAttribute(
		'href',
		'/system-health'
	);
	await expect(section.getByRole('link', { name: 'Logs' })).toHaveAttribute(
		'href',
		'/settings/logs'
	);
	await expect(section.getByRole('link', { name: 'Metrics' })).toHaveAttribute('href', '/metrics');
	for (const absent of [
		'https://home.lan:8123',
		'mqtt://home.lan:1883',
		'https://automation.lan/hooks/kp',
		'https://ops.example.com/kp',
		'1,402 MESSAGES TODAY',
		'SCRAPED 11s AGO'
	]) {
		await expect(section.getByText(absent, { exact: true })).toHaveCount(0);
	}
	await expect(section.getByText(/kp_ha_[a-z0-9]+/)).toHaveCount(0);
	expect(writes).toEqual([]);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});
