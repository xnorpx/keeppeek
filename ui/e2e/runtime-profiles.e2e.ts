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

test('administrator creates, renames, activates, returns to Base, and deletes a profile', async ({
	page
}) => {
	const control = await mockControlPeer(page, { runtimeConfiguration: config });
	await page.goto('/settings#runtime-profiles');

	const section = page.locator('#runtime-profiles');
	await expect(section).toContainText('Active policy: Base');
	await section.getByLabel('Create a profile').fill('Away');
	await section.getByRole('button', { name: 'Create profile' }).click();
	await section.getByLabel('Display name').fill('Night');
	await section.getByRole('button', { name: 'Save profile changes' }).click();
	await expect(section.getByRole('status')).toContainText('Runtime profile changes saved.');

	await section.getByRole('button', { name: 'Activate', exact: true }).click();
	await expect(section.getByRole('status')).toContainText('Activated Night.');
	await expect.poll(() => control.runtimeProfileWrites.length).toBe(2);
	expect(control.runtimeProfileWrites[0]).toMatchObject({
		key: 'registry',
		expectedRevision: 1n
	});
	expect(control.runtimeProfileWrites[0]!.value.profiles).toMatchObject({
		away: { id: 'away', name: 'Night' }
	});
	expect(control.runtimeProfileWrites[1]).toMatchObject({
		key: 'active',
		expectedRevision: 1n,
		value: { profile_id: 'away' }
	});

	await page.reload();
	await expect(section).toContainText('Active policy: Night');
	await section.getByRole('button', { name: 'Select Base' }).click();
	await expect(section.getByRole('status')).toContainText('Base policy selected.');
	await page.reload();
	await expect(section).toContainText('Active policy: Base');
	await section.getByRole('button', { name: 'Delete', exact: true }).click();
	await section.getByRole('button', { name: 'Save profile changes' }).click();
	await expect(section.getByLabel('Edit profile')).toHaveCount(0);
	await expect.poll(() => control.runtimeProfileWrites.length).toBe(4);
	expect(control.runtimeProfileWrites[2]!.value).toEqual({ profile_id: '' });
	expect(control.runtimeProfileWrites[3]!.value.profiles).toEqual({});
});

test('User sees profile state but has no profile mutation or activation controls', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockControlPeer(page, {
		accessRole: 'user',
		runtimeConfiguration: config,
		capabilityIds: ['keeppeek.runtime-profiles.v1']
	});
	await page.goto('/');

	await expect(
		page.getByRole('link', { name: 'Active camera profile Base; 0 cameras degraded' })
	).toBeVisible();
	await expect(page.locator('#runtime-profiles')).toHaveCount(0);
});
