import { expect, test, type Locator, type Route } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';
import { startHomeAssistantServer } from './fixtures/home-assistant-server';

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

function isolatedBackendRoute(origin: string, backend: string) {
	return async (route: Route) => {
		const url = new URL(route.request().url());
		if (
			url.origin === origin &&
			(url.pathname === '/create' ||
				url.pathname.startsWith('/api/') ||
				url.pathname.startsWith('/auth/') ||
				url.pathname.startsWith('/config/') ||
				url.pathname === '/metrics' ||
				url.pathname === '/logs')
		) {
			const response = await route.fetch({ url: `${backend}${url.pathname}${url.search}` });
			await route.fulfill({ response });
		} else await route.continue();
	};
}

async function expectLifecyclePolicy(section: Locator, profile: boolean) {
	await expect(section).toContainText(`Active policy: ${profile ? 'Lifecycle Away' : 'Base'}`);
	const camera = section
		.getByRole('region', { name: 'Runtime profile convergence' })
		.getByRole('listitem')
		.filter({ hasText: '192.0.2.101' });
	await expect(camera.getByLabel('applied policy application')).toBeVisible();
	await expect(camera).toContainText(profile ? 'Unavailable (profile)' : 'Available (base)');
	await expect(camera).toContainText(profile ? 'privacy enforced (profile)' : 'privacy off (base)');
	await expect(camera).toContainText(
		profile ? 'notifications suppressed (profile)' : 'notifications enabled (base)'
	);
	if (profile) await expect(camera).toContainText('recording off (profile)');
	else await expect(camera).toContainText(/recording \S+ \(base\)/);
}

async function conflictLifecycleEditors(writer: Locator, reader: Locator) {
	await reader
		.getByRole('combobox', { name: 'Edit profile', exact: true })
		.selectOption('lifecycle-away');
	await writer.getByLabel('Display name').fill('My unsaved label');
	await reader
		.getByRole('combobox', { name: 'Recording', exact: true })
		.nth(1)
		.selectOption('main');
	await reader.getByRole('button', { name: 'Save profile changes' }).click();
	await expect(reader.getByRole('status')).toContainText('Runtime profile changes saved.');
	await writer.getByRole('button', { name: 'Save profile changes' }).click();
	await expect(writer.getByRole('alert')).toBeVisible();
	await expect(writer.getByLabel('Display name')).toHaveValue('My unsaved label');
	await writer.getByRole('button', { name: 'Discard changes and reload' }).click();
	await expect(writer.getByLabel('Display name')).toHaveValue('Lifecycle Away');
}

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

test.describe.configure({ mode: 'serial' });

test('two Settings clients restore active runtime policy after reconnect and recorder restart', async ({
	page,
	context,
	baseURL
}) => {
	test.setTimeout(120_000);
	if (!baseURL) throw new Error('Runtime-profile lifecycle requires the configured UI origin.');
	const fixture = await startHomeAssistantServer(new URL(baseURL).origin, { trustedLocal: true });
	const routeBackend = isolatedBackendRoute(new URL(baseURL).origin, fixture.url);
	await context.route('**/*', routeBackend);
	const reader = await context.newPage();
	const profiles = page.locator('#runtime-profiles');
	const other = reader.locator('#runtime-profiles');
	try {
		await Promise.all([
			page.goto('/settings#runtime-profiles'),
			reader.goto('/settings#runtime-profiles')
		]);
		await expect(profiles).toContainText('Active policy: Base');
		await profiles.getByLabel('Create a profile').fill('Lifecycle Away');
		await profiles.getByRole('button', { name: 'Create profile' }).click();
		await profiles
			.getByRole('combobox', { name: 'Availability', exact: true })
			.first()
			.selectOption('false');
		await profiles
			.getByRole('combobox', { name: 'Recording', exact: true })
			.first()
			.selectOption('off');
		await profiles
			.getByRole('combobox', { name: 'Privacy', exact: true })
			.first()
			.selectOption('true');
		await profiles
			.getByRole('combobox', { name: 'Notifications', exact: true })
			.first()
			.selectOption('true');
		await profiles.getByRole('button', { name: 'Save profile changes' }).click();
		await expect(profiles.getByRole('status')).toContainText('Runtime profile changes saved.');
		await conflictLifecycleEditors(profiles, other);
		await profiles.getByRole('button', { name: 'Activate', exact: true }).click();
		await expect(profiles.getByRole('status')).toContainText('Activated Lifecycle Away.');
		for (const section of [profiles, other]) await expectLifecyclePolicy(section, true);
		const accepted = await profiles.getByText(/Active policy:/).textContent();
		await expect(other.getByText(/Active policy:/)).toHaveText(accepted!);
		await reader.reload();
		await expect(other.getByText(/Active policy:/)).toHaveText(accepted!);

		await fixture.restart();
		await Promise.all([page.reload(), reader.reload()]);
		for (const section of [profiles, other]) {
			await expect(section.getByText(/Active policy:/)).toHaveText(accepted!);
			await expectLifecyclePolicy(section, true);
		}
		await profiles.getByRole('button', { name: 'Select Base' }).click();
		await expect(profiles.getByRole('status')).toContainText('Base policy selected.');
		for (const section of [profiles, other]) await expectLifecyclePolicy(section, false);
		const base = await profiles.getByText(/Active policy:/).textContent();
		await expect(other.getByText(/Active policy:/)).toHaveText(base!);
		expect(BigInt(base!.match(/revision (\d+)/)![1]!)).toBeGreaterThan(
			BigInt(accepted!.match(/revision (\d+)/)![1]!)
		);
	} finally {
		try {
			await reader.close();
			await context.unroute('**/*', routeBackend);
		} finally {
			await fixture.close();
		}
	}
});

test('authorized WebRTC profile activation reaches another connected client', async ({ page }) => {
	test.setTimeout(60_000);
	await page.goto('/');
	await expect(page.getByRole('heading', { name: 'Dashboard', exact: true })).toBeVisible();
	const result = await page.evaluate(async () => {
		const modulePath = '/e2e/fixtures/runtime-profiles-browser.ts';
		const fixture = (await import(
			modulePath
		)) as typeof import('./fixtures/runtime-profiles-browser');
		return fixture.activateFromAnotherClient();
	});
	expect(result.activeProfileId).toMatch(/^e2e-[a-f0-9-]+$/);
	expect(result.observed).toContainEqual({
		profileId: result.activeProfileId,
		revision: result.activatedRevision
	});
	expect(result.readerActive).toEqual({
		profileId: result.activeProfileId,
		revision: result.activatedRevision
	});
});

test('Administrator creates, activates, and removes a profile through Settings', async ({
	page
}) => {
	test.setTimeout(60_000);
	await page.goto('/settings');
	const profiles = page.locator('#runtime-profiles');
	await expect(profiles.getByRole('heading', { name: 'Runtime profiles' })).toBeVisible();
	await profiles.getByLabel('Create a profile').fill('E2E Away');
	await profiles.getByRole('button', { name: 'Create profile' }).click();
	await expect(profiles.getByText('Stable profile ID: e2e-away', { exact: true })).toBeVisible();
	await profiles.getByRole('button', { name: 'Save profile changes' }).click();
	await expect(profiles.getByRole('status')).toContainText('Runtime profile changes saved.');
	await profiles.getByRole('button', { name: 'Activate' }).click();
	await expect(profiles.getByRole('status')).toContainText('Activated E2E Away.');
	await expect(profiles.getByText(/Active policy: E2E Away/)).toBeVisible();
	await profiles.getByRole('button', { name: 'Select Base' }).click();
	await expect(profiles.getByRole('status')).toContainText('Base policy selected.');
	await expect(profiles.getByText(/Active policy: Base/)).toBeVisible();
	await profiles.getByRole('button', { name: 'Delete' }).click();
	await profiles.getByRole('button', { name: 'Save profile changes' }).click();
	await expect(profiles.getByLabel('Edit profile')).toHaveCount(0);
});

test('Administrator manages runtime profiles on mobile with the keyboard', async ({ page }) => {
	test.setTimeout(60_000);
	await page.setViewportSize({ width: 390, height: 844 });
	await page.goto('/settings');
	await page.getByRole('link', { name: /Runtime profiles/ }).click();
	const profiles = page.locator('#runtime-profiles');
	await expect(profiles.getByRole('heading', { name: 'Runtime profiles' })).toBeVisible();
	await profiles.getByLabel('Create a profile').fill('Mobile Away');
	await profiles.getByRole('button', { name: 'Create profile' }).press('Enter');
	await expect(profiles.getByText('Stable profile ID: mobile-away', { exact: true })).toBeVisible();
	await profiles.getByRole('button', { name: 'Save profile changes' }).press('Enter');
	await expect(profiles.getByRole('status')).toContainText('Runtime profile changes saved.');
	await profiles.getByRole('button', { name: 'Activate' }).press('Space');
	await expect(profiles.getByRole('status')).toContainText('Activated Mobile Away.');
	await expect(profiles.getByText(/Active policy: Mobile Away/)).toBeVisible();
	await profiles.getByRole('button', { name: 'Select Base' }).press('Enter');
	await expect(profiles.getByRole('status')).toContainText('Base policy selected.');
	await profiles.getByRole('button', { name: 'Delete' }).click();
	await expect(profiles.getByRole('option', { name: 'Mobile Away' })).toHaveCount(0);
	await profiles.getByRole('button', { name: 'Save profile changes' }).click();
	await expect(profiles.getByRole('option', { name: 'Mobile Away' })).toHaveCount(0);
});
