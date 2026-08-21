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

test('Board 18 gates notification rules without inventing firing or delivery history', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const writes: string[] = [];
	let permissionRequests = 0;
	await page.addInitScript(() => {
		class MockNotification {
			static permission: NotificationPermission = 'default';
			static requestPermission(): Promise<NotificationPermission> {
				window.dispatchEvent(new Event('notification-permission-requested'));
				return Promise.resolve('default');
			}
		}
		Object.defineProperty(window, 'Notification', { value: MockNotification });
	});
	await page.exposeFunction('recordNotificationPermissionRequest', () => {
		permissionRequests += 1;
	});
	await page.addInitScript(() => {
		window.addEventListener('notification-permission-requested', () => {
			void (
				window as typeof window & {
					recordNotificationPermissionRequest: () => Promise<void>;
				}
			).recordNotificationPermissionRequest();
		});
	});
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

	await page.goto('/settings#notifications');

	const section = page.getByRole('region', { name: 'A notification you ignore is a bug' });
	await expect(section).toBeInViewport();
	expect(
		await section.evaluate((element) => element.getBoundingClientRect().width)
	).toBeGreaterThan(1200);
	await expect(section).toContainText('NOTIFICATIONS · TARGET');
	await expect(section.getByText('Server update required · keeppeek.rules.v1')).toHaveCount(3);
	await expect(section.getByText('UNAVAILABLE', { exact: true })).toHaveCount(4);

	for (const channel of ['Push to phones', 'Email', 'In this browser', 'MQTT and webhooks']) {
		await expect(section.getByText(channel, { exact: true })).toBeVisible();
	}
	for (const field of [
		'Event or health condition',
		'Camera or group scope',
		'Human-facing destinations',
		'Cooldown and rate limit',
		'Quiet-hours policy and critical bypass'
	]) {
		await expect(section.getByText(field, { exact: true })).toBeVisible();
	}
	await expect(section).toContainText('NOT ENFORCED');
	await expect(section).toContainText('RULES RUNTIME UNAVAILABLE');
	for (const label of ['Configured rules', 'Fired in last 7 days', 'Quiet hours', 'Retry queue']) {
		await expect(section.getByText(label, { exact: true }).locator('..')).toContainText(
			'Unavailable'
		);
	}
	await expect(section.getByRole('link', { name: 'Open integration contracts' })).toHaveAttribute(
		'href',
		'/settings#integrations'
	);

	for (const absent of [
		'Pushover',
		'Last delivered 41m ago',
		'Permission granted',
		'219',
		'3,190',
		'22:00 – 06:30',
		'Person at Front Door'
	]) {
		await expect(section.getByText(absent, { exact: true })).toHaveCount(0);
	}
	expect(permissionRequests).toBe(0);
	expect(writes).toEqual([]);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});
