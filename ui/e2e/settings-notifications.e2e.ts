import { expect, test } from '@playwright/test';
import {
	createNotificationRule,
	type NotificationHistoryGroup,
	type NotificationInbox,
	type NotificationRuleRecord
} from '../src/lib/notifications';
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

function notificationFixtures(): {
	rules: NotificationRuleRecord[];
	inbox: NotificationInbox;
	history: NotificationHistoryGroup[];
} {
	const now = Date.UTC(2026, 7, 25, 12, 0);
	const rule = createNotificationRule('front-door-person', 'Europe/Stockholm');
	rule.name = 'Front door person';
	const record: NotificationRuleRecord = {
		id: rule.id,
		ownerId: 'administrator',
		active: structuredClone(rule),
		activeRevision: 3n,
		draft: structuredClone(rule),
		draftRevision: 4n,
		createdAtMs: now - 86_400_000,
		updatedAtMs: now - 60_000,
		lastMatchAtMs: now - 45_000,
		lastDeliveryAtMs: now - 44_000
	};
	const item = {
		logicalId: 'notification-event-1',
		ruleId: rule.id,
		sourceId: 'front-door',
		sourceIdentity: 'event-1',
		lifecycle: 'event',
		stage: 'enriched' as const,
		revision: 2n,
		title: 'Person at front door',
		body: 'Open the event',
		deepLink: '/events?camera=front-door&event=event-1',
		attachmentAvailable: true,
		canonicalAttachment: {
			id: 'snapshot-1',
			type: 'snapshot',
			content_type: 'image/jpeg',
			byte_length: 128,
			ordinal: 0,
			timestamp_ms: now - 45_000
		},
		iconKey: 'person' as const,
		imageAvailability: 'available' as const,
		severity: 'info' as const,
		createdAtMs: now - 45_000,
		updatedAtMs: now - 44_000,
		seenAtMs: null,
		acknowledgedAtMs: null
	};
	return {
		rules: [record],
		inbox: { items: [item], unreadCount: 1n },
		history: [
			{
				notification: item,
				events: [
					{
						sequence: 1n,
						revision: 1n,
						stage: 'preliminary',
						outcome: 'created',
						reason: null,
						occurredAtMs: now - 45_000,
						nextEligibleAtMs: null
					}
				],
				attempts: [
					{
						sequence: 1n,
						channel: 'browser',
						stage: 'preliminary',
						attempt: 1,
						outcome: 'delivered',
						targetHash: '0123456789abcdef0123456789abcdef',
						providerStatus: null,
						providerRequestId: null,
						providerAcknowledgedAtMs: null,
						providerExpiredAtMs: null,
						providerAcknowledgedByHash: null,
						providerAcknowledgementState: null,
						reason: null,
						attemptedAtMs: now - 44_000,
						retryAtMs: null
					}
				]
			}
		]
	};
}

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

test('notification rules expose live editor, inbox, history, tests, and conflicts', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 1000 });
	const fixtures = notificationFixtures();
	const requests = await mockControlPeer(page, {
		runtimeConfiguration: config,
		health: { system: { disks: [] }, storage: { catalog: null } },
		capabilityIds: ['keeppeek.rules.v1'],
		notificationRules: fixtures.rules,
		notificationInbox: fixtures.inbox,
		notificationHistory: fixtures.history,
		notificationConflictOnSave: true
	});

	await page.goto('/settings#notifications');
	const section = page.getByRole('region', { name: 'Notification rules' });
	await expect(section).toBeInViewport();
	await expect(section).toContainText('NOTIFICATIONS · LIVE');
	await expect(section).toContainText('1/1 ACTIVE');
	await expect(section).toContainText('1 UNREAD');
	await expect(section.getByText('Front door person', { exact: true })).toBeVisible();
	await expect(section).toContainText('r3 active · r4 draft');

	await section.getByRole('button', { name: 'Test Front door person' }).click();
	await expect(section).toContainText('Test queued 1 channel attempt.');
	await expect.poll(() => requests.notificationActions).toContain('test');

	await section.getByRole('button', { name: 'Edit Front door person' }).click();
	const editor = section.getByRole('dialog');
	await expect(editor).toBeVisible();
	await editor.getByLabel('Rule name').fill('Front entrance person');
	await editor.getByRole('button', { name: 'Save draft' }).click();
	await expect(editor).toContainText('Your draft remains open.');
	await expect(editor.getByLabel('Rule name')).toHaveValue('Front entrance person');

	await editor.getByRole('button', { name: 'Close rule editor' }).click();
	await section.getByRole('tab', { name: /Inbox/ }).click();
	await expect(section.getByText('Person at front door', { exact: true })).toBeVisible();
	await section.getByRole('button', { name: 'Mark Person at front door seen' }).click();
	await expect.poll(() => requests.notificationActions).toContain('markSeen');

	await section.getByRole('tab', { name: 'History' }).click();
	await section.getByText('Person at front door', { exact: true }).click();
	await expect(section).toContainText('created');
	await expect(section).toContainText('browser · delivered');
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('notification rule editor remains usable on mobile', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const fixtures = notificationFixtures();
	await mockControlPeer(page, {
		runtimeConfiguration: config,
		health: { system: { disks: [] }, storage: { catalog: null } },
		capabilityIds: ['keeppeek.rules.v1'],
		notificationRules: fixtures.rules,
		notificationInbox: fixtures.inbox,
		notificationHistory: fixtures.history
	});

	await page.goto('/settings#notifications');
	const section = page.getByRole('region', { name: 'Notification rules' });
	await expect(section).toBeVisible();
	await section.getByRole('button', { name: 'Add rule' }).click();
	const editor = section.getByRole('dialog');
	await expect(editor).toBeVisible();
	await expect(editor.getByLabel('Rule name')).toBeVisible();
	await expect(editor.getByText('EFFECTIVE POLICY', { exact: true })).toBeVisible();
	await expect(editor.getByRole('button', { name: 'Save & activate' })).toBeVisible();
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});
