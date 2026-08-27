import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { ControlClient } from '$lib/control-client';
import {
	createNotificationRule,
	type NotificationHistoryGroup,
	type NotificationInbox,
	type NotificationRuleRecord
} from '$lib/notifications';
import NotificationsRuntimeFixture from './NotificationsRuntime.fixture.svelte';

function ruleRecord(): NotificationRuleRecord {
	const rule = createNotificationRule('front-door-person', 'Europe/Stockholm');
	rule.name = 'Front door person';
	rule.actions.push({
		enabled: true,
		channel: 'webhook',
		destination: '',
		destination_configured: true,
		destination_ref: '0123456789abcdef'.repeat(4),
		template: { title: 'Person', body: 'Open {{notification.deep_link}}' },
		attachment: 'never',
		allow_second_delivery: false
	});
	return {
		id: rule.id,
		ownerId: 'administrator',
		active: structuredClone(rule),
		activeRevision: 3n,
		draft: structuredClone(rule),
		draftRevision: 4n,
		createdAtMs: Date.UTC(2026, 7, 25, 10),
		updatedAtMs: Date.UTC(2026, 7, 25, 11),
		lastMatchAtMs: Date.UTC(2026, 7, 25, 11, 30),
		lastDeliveryAtMs: Date.UTC(2026, 7, 25, 11, 31)
	};
}

function inbox(): NotificationInbox {
	return {
		unreadCount: 1n,
		items: [
			{
				logicalId: 'notification-1',
				ruleId: 'front-door-person',
				sourceId: 'front-door',
				lifecycle: 'event',
				stage: 'enriched',
				revision: 2n,
				title: 'Person at front door',
				body: 'Open the event',
				deepLink: '/events?camera=front-door&event=event-1',
				attachmentAvailable: true,
				severity: 'info',
				createdAtMs: Date.UTC(2026, 7, 25, 11, 30),
				updatedAtMs: Date.UTC(2026, 7, 25, 11, 31),
				seenAtMs: null,
				acknowledgedAtMs: null
			}
		]
	};
}

function history(): NotificationHistoryGroup[] {
	const notification = inbox().items[0]!;
	return [
		{
			notification,
			events: [
				{
					sequence: 1n,
					revision: 1n,
					stage: 'preliminary',
					outcome: 'created',
					reason: null,
					occurredAtMs: notification.createdAtMs,
					nextEligibleAtMs: null
				}
			],
			attempts: [
				{
					sequence: 1n,
					channel: 'push',
					stage: 'preliminary',
					attempt: 1,
					outcome: 'delivered',
					targetHash: '0123456789abcdef',
					providerStatus: 200,
					providerRequestId: '647d2300-702c-4b38-8b2f-d56326ae460b',
					providerAcknowledgedAtMs: null,
					providerExpiredAtMs: null,
					providerAcknowledgedByHash: null,
					providerAcknowledgementState: 'pending',
					reason: null,
					attemptedAtMs: notification.createdAtMs,
					retryAtMs: null
				}
			]
		}
	];
}

describe('NotificationsRuntime', () => {
	it('renders server evidence, updates receipts, and opens the editor', async () => {
		const markNotificationSeen = vi.fn().mockResolvedValue(undefined);
		const client = {
			listNotificationRules: vi.fn().mockResolvedValue([ruleRecord()]),
			getNotificationInbox: vi.fn().mockResolvedValue(inbox()),
			getNotificationHistory: vi.fn().mockResolvedValue(history()),
			markNotificationSeen,
			acknowledgeNotification: vi.fn().mockResolvedValue(undefined),
			clearNotification: vi.fn().mockResolvedValue(undefined),
			clearNotifications: vi.fn().mockResolvedValue(1n),
			saveNotificationRuleDraft: vi.fn(),
			activateNotificationRule: vi.fn(),
			deleteNotificationRule: vi.fn(),
			testNotificationRule: vi.fn()
		} as unknown as ControlClient;

		await render(NotificationsRuntimeFixture, { props: { client } });
		await expect.element(page.getByText('Front door person', { exact: true })).toBeVisible();
		await expect.element(page.getByText('r3 active · r4 draft', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Awaiting acknowledgement', { exact: true })).toBeVisible();

		await userEvent.click(page.getByRole('tab', { name: /Inbox/ }));
		await expect.element(page.getByText('Person at front door', { exact: true })).toBeVisible();
		await userEvent.click(page.getByRole('button', { name: 'Mark Person at front door seen' }));
		expect(markNotificationSeen).toHaveBeenCalledWith('notification-1');

		await userEvent.click(page.getByRole('tab', { name: 'Rules' }));
		await userEvent.click(page.getByRole('button', { name: 'Edit Front door person' }));
		await expect.element(page.getByRole('dialog')).toBeVisible();
		await expect.element(page.getByLabelText('Rule name')).toHaveValue('Front door person');
		await expect
			.element(page.getByLabelText('Webhook URL'))
			.toHaveAttribute('placeholder', 'Configured');

		await userEvent.selectOptions(page.getByLabelText('Channel').first(), 'push');
		await expect
			.element(page.getByLabelText('Application token'))
			.toHaveAttribute('type', 'password');
		await expect
			.element(page.getByLabelText('User or group key'))
			.toHaveAttribute('type', 'password');
		await expect.element(page.getByLabelText('Device names')).toBeVisible();
		await expect.element(page.getByLabelText('Sound')).toBeVisible();
		await expect.element(page.getByLabelText('Deep-link base URL')).toBeVisible();
		await userEvent.selectOptions(page.getByLabelText('Priority'), '2');
		await expect.element(page.getByLabelText('Emergency retry (seconds)')).toHaveValue(30);
		await expect.element(page.getByLabelText('Emergency expiry (seconds)')).toHaveValue(300);

		await userEvent.click(page.getByRole('tab', { name: 'History' }));
		await userEvent.click(page.getByText('Person at front door', { exact: true }));
		await expect.element(page.getByText(/request 647d2300-702/)).toBeVisible();
		await expect.element(page.getByText(/pending/)).toBeVisible();
	});
});
