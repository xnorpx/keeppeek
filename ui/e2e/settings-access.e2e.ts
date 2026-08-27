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

test('manages enforced roles, credentials, sessions, and audit records', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const writes: string[] = [];
	page.on('request', (request) => {
		const pathname = new URL(request.url()).pathname;
		if (request.method() !== 'GET' && pathname !== '/create' && pathname !== '/delete') {
			writes.push(`${request.method()} ${request.url()}`);
		}
	});
	const controls = await mockControlPeer(page, {
		runtimeConfiguration: config,
		health: { system: { disks: [] }, storage: { catalog: null } },
		capabilityIds: ['keeppeek.identity.v1']
	});

	await page.goto('/settings#access');

	const section = page.getByRole('region', { name: 'Remote access and roles' });
	await expect(section).toBeInViewport();
	expect(
		await section.evaluate((element) => element.getBoundingClientRect().width)
	).toBeGreaterThan(1200);
	await expect(section).toContainText('Server-authoritative access policy');
	await expect(section).toContainText('Enforced centrally by the server');
	await expect(section).toContainText('TARGET · IDENTITY.V1');

	for (const action of [
		'Watch live video',
		'Open stored recordings',
		'Operate camera PTZ and presets',
		'Join a group and publish local media',
		'Export a clip or still',
		'Configure cameras',
		'Configure storage and services',
		'Manage identities and tokens'
	]) {
		await expect(section.getByText(action, { exact: true })).toBeVisible();
	}
	await expect(section.locator('[aria-label="Administrator target allows"]')).toHaveCount(8);
	await expect(section.locator('[aria-label="User target allows"]')).toHaveCount(4);
	await expect(section.locator('[aria-label="User target excludes"]')).toHaveCount(4);

	await expect(section).toContainText('Access credentials');
	await expect(section).toContainText('Initial Administrator');
	await expect(section).toContainText('Active sessions');
	await expect(section).toContainText('Local Administrator');
	await expect(section).toContainText('Security audit');
	await expect(section).toContainText('session create');

	await section.getByRole('button', { name: 'Retrieve initial key' }).click();
	await expect(
		section.getByText('550e8400-e29b-41d4-a716-446655440000', { exact: true })
	).toBeVisible();
	expect(controls.accessKeyReveals).toBe(1);

	await section.getByRole('button', { name: 'New credential' }).click();
	await section.getByLabel('Name').fill('Workshop tablet');
	await section.getByLabel('Description').fill('Shared review station');
	await section.getByRole('button', { name: 'Create', exact: true }).click();
	await expect(section.getByText('Workshop tablet', { exact: true })).toBeVisible();
	await expect(
		section.getByText('550e8400-e29b-41d4-a716-446655440002', { exact: true })
	).toBeVisible();
	expect(controls.accessCredentialCreates).toBe(1);
	await expect(section.locator('input[type="password"]')).toHaveCount(0);
	expect(writes).toEqual([]);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});

test('renders credential and session management on mobile without horizontal overflow', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockControlPeer(page, {
		runtimeConfiguration: config,
		health: { system: { disks: [] }, storage: { catalog: null } },
		capabilityIds: ['keeppeek.identity.v1']
	});

	await page.goto('/settings#access');

	const header = page.locator('[data-mobile-settings-header]');
	const section = page.locator('[data-mobile-access]');
	const actionBar = page.locator('[data-mobile-settings-action-bar]');
	await expect(header).toContainText('Access');
	await expect(header).toContainText('Target · identity v1');
	await expect(section).toContainText('Access policy active');
	await expect(section).toContainText('Initial Administrator');
	await expect(section).toContainText('Active sessions');
	await expect(section).toContainText('Security audit');
	await expect(actionBar.getByRole('button', { name: 'New token' })).toBeEnabled();
	await expect(page.locator('[data-shell-mobile-nav]')).toHaveCount(0);
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});
