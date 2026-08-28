import { expect, test } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';
import type { MqttIntegration } from '../src/lib/integrations';

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

test('Board 17 configures and observes the MQTT 5 event forwarder', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const writes: string[] = [];
	const mqtt: MqttIntegration = {
		configuration: {
			enabled: true,
			broker_url: 'mqtt://broker.home:1883',
			client_id: 'keeppeek',
			instance_id: 'home-nvr',
			forwarder_id: 'mqtt',
			topic_prefix: 'keeppeek',
			username: 'operator',
			password_configured: true,
			tls_ca_path: null,
			qos: 1,
			retain_events: false,
			retain_health: true,
			outbox_max_mb: 64,
			retry_min_ms: 250,
			retry_max_ms: 30_000
		},
		status: {
			enabled: true,
			state: 'connected',
			detail: 'MQTT 5 broker is connected.',
			connected_at_ms: 1_786_800_000_000,
			last_received_at_ms: 1_786_800_001_000,
			last_delivered_at_ms: 1_786_800_002_000,
			pending_items: 2,
			pending_bytes: 2048,
			oldest_unacknowledged_timestamp_ms: 1_786_800_001_000,
			retry_count: 1,
			duplicate_count: 0,
			outbox_limit_bytes: 67_108_864
		},
		configuration_revision: '1'
	};
	page.on('request', (request) => {
		const pathname = new URL(request.url()).pathname;
		if (request.method() !== 'GET' && pathname !== '/create' && pathname !== '/delete') {
			writes.push(`${request.method()} ${request.url()}`);
		}
	});
	const peerOptions = {
		runtimeConfiguration: config,
		health: { system: { disks: [] }, storage: { catalog: null } },
		mqttIntegration: mqtt,
		mqttUpdateResult: {
			...mqtt,
			configuration_revision: '2'
		},
		mqttTestResult: {
			ok: true,
			kind: null,
			detail: 'Connected and published a test status to the MQTT 5 broker.'
		},
		capabilityIds: ['keeppeek.mqtt-forwarder.v1']
	};
	const requests = await mockControlPeer(page, peerOptions);

	await page.goto('/settings#integrations');

	const section = page.getByRole('region', {
		name: 'Everything has an explicit egress boundary'
	});
	await expect(section).toBeInViewport();
	expect(
		await section.evaluate((element) => element.getBoundingClientRect().width)
	).toBeGreaterThan(1200);
	await expect(section).toContainText('NO THIRD-PARTY MEDIA RELAY');
	await expect(section.getByText('RUNTIME UNAVAILABLE')).toHaveCount(3);
	await expect(section.getByRole('button', { name: 'Configuration unavailable' })).toHaveCount(3);

	for (const integration of ['Home Assistant', 'MQTT 5', 'Webhooks', 'Prometheus']) {
		await expect(section.getByText(integration, { exact: true })).toBeVisible();
	}
	await expect(section).toContainText('Committed event revisions and camera health transitions');
	await expect(section.getByText('MISSING CONTRACTS')).toHaveCount(3);
	await expect(section).toContainText('direct card package');
	await expect(section).toContainText('durable retry queue');
	await expect(section).toContainText('scrape configuration UI');
	await expect(section.getByText('/metrics', { exact: true })).toBeVisible();

	await expect(section).toContainText(/connected · MQTT 5 · 2 QUEUED/i);
	await expect(section.getByText('mqtt://broker.home:1883', { exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Edit broker' }).click();
	await expect(page.getByLabel('Broker URL')).toHaveValue('mqtt://broker.home:1883');
	await page.getByLabel('Password', { exact: true }).fill('replacement-secret');
	await page.getByRole('button', { name: 'Save MQTT settings' }).click();
	await expect(section.getByRole('status')).toContainText('MQTT settings saved and applied.');
	await expect(page.getByLabel('Password', { exact: true })).toHaveCount(0);
	await expect(section).not.toContainText('replacement-secret');
	await page.getByRole('button', { name: 'Edit broker' }).click();
	await expect(page.getByLabel('Password', { exact: true })).toHaveValue('');
	await page.getByRole('button', { name: 'Test connection' }).click();
	await expect(section.getByRole('status')).toContainText(
		'Connected and published a test status to the MQTT 5 broker.'
	);
	peerOptions.mqttUpdateResult = {
		...mqtt,
		configuration: { ...mqtt.configuration, enabled: false },
		status: {
			...mqtt.status,
			enabled: false,
			state: 'disabled',
			detail: 'MQTT 5 event forwarding is disabled.'
		},
		configuration_revision: '3'
	};
	await page.getByLabel('Enabled').uncheck();
	await page.getByRole('button', { name: 'Save MQTT settings' }).click();
	await expect(section.getByRole('status')).toContainText(
		'MQTT forwarding disabled. Queued events remain durable.'
	);
	await expect(section).toContainText(/disabled · MQTT 5/i);
	expect(requests.mqttUpdates).toHaveLength(2);
	expect(requests.mqttTests).toHaveLength(1);
	expect(requests.mqttUpdates[0]?.password).toBe('replacement-secret');
	expect(requests.mqttUpdates[1]?.enabled).toBe(false);
	expect(JSON.stringify(mqtt)).not.toContain('replacement-secret');
	expect(writes).toEqual([]);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});
