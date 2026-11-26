import { readFile } from 'node:fs/promises';
import { expect, test } from '@playwright/test';

test('real activity analytics summarizes seeded events, toggles accessible table, exports, and drills down to Events', async ({
	page
}) => {
	test.setTimeout(90_000);
	await page.setViewportSize({ width: 1440, height: 900 });
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('console', (message) => {
		if (message.type() === 'error') errors.push(message.text());
	});

	await page.goto('/events');
	const activityLink = page.getByRole('link', { name: 'Activity summary', exact: true });
	await expect(activityLink).toBeVisible({ timeout: 30_000 });

	const fixture = await page.evaluate(async () => {
		const modulePath = '/e2e/fixtures/event-workflow-browser.ts';
		const helpers = (await import(
			modulePath
		)) as typeof import('./fixtures/event-workflow-browser');
		return helpers.seedEventWorkflow(6, Date.now() - 2 * 3_600_000);
	});

	await activityLink.click();
	await expect(page).toHaveURL(/\/activity/);
	await expect(page.getByRole('heading', { name: 'Activity summary', exact: true })).toBeVisible();
	await expect(page.getByRole('note')).toContainText(
		/Counts reflect producer-reported event metadata/i
	);

	// Wait for the real server summary to finish loading and include our seeded zone.
	await expect(page.getByText(fixture.zone)).toBeVisible({ timeout: 15_000 });

	// Switch trend chart to accessible table view and verify UTC provenance + drilldown link.
	await page.getByRole('button', { name: 'Table', exact: true }).click();
	const tableLinks = page.getByRole('link', { name: 'Open matching events' });
	await expect(tableLinks.first()).toBeVisible();

	// Verify CSV and JSON export downloads include provenance, timezone, and UTC metadata.
	const [csvDownload] = await Promise.all([
		page.waitForEvent('download'),
		page.getByRole('button', { name: 'Export CSV', exact: true }).click()
	]);
	expect(csvDownload.suggestedFilename()).toMatch(/^keeppeek-activity-.*\.csv$/);
	const csvPath = await csvDownload.path();
	if (!csvPath) throw new Error('Expected CSV download path.');
	const csvText = await readFile(csvPath, 'utf8');
	expect(csvText).toContain('producer-reported event metadata');
	expect(csvText).toContain('# timezone,UTC');
	expect(csvText).toContain(fixture.zone);

	const [jsonDownload] = await Promise.all([
		page.waitForEvent('download'),
		page.getByRole('button', { name: 'Export JSON', exact: true }).click()
	]);
	expect(jsonDownload.suggestedFilename()).toMatch(/^keeppeek-activity-.*\.json$/);
	const jsonPath = await jsonDownload.path();
	if (!jsonPath) throw new Error('Expected JSON download path.');
	const jsonPayload = JSON.parse(await readFile(jsonPath, 'utf8')) as {
		provenanceNotice: string;
		timezone: string;
		tzdbVersion: string;
		utcRange: { start: string; end: string };
	};
	expect(jsonPayload.provenanceNotice).toMatch(/producer-reported event metadata/i);
	expect(jsonPayload.timezone).toBe('UTC');
	expect(jsonPayload.tzdbVersion.length).toBeGreaterThan(0);
	expect(jsonPayload.utcRange.start).toMatch(/Z$/);
	expect(jsonPayload.utcRange.end).toMatch(/Z$/);

	// Change display timezone to both DST and sub-hour IANA zones and confirm exact server responses.
	await page.getByLabel('Display timezone (IANA)').selectOption('America/New_York');
	await expect(page.getByText(/Timezone: America\/New_York/i)).toBeVisible({ timeout: 15_000 });
	await page.getByLabel('Display timezone (IANA)').selectOption('Asia/Kolkata');
	await expect(page.getByText(/Timezone: Asia\/Kolkata/i)).toBeVisible({ timeout: 15_000 });
	await expect(page.getByText(fixture.zone)).toBeVisible({ timeout: 15_000 });

	// Verify mobile layout renders without horizontal overflow or missing controls.
	await page.setViewportSize({ width: 390, height: 844 });
	await expect(page.getByRole('heading', { name: 'Activity summary', exact: true })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Export CSV', exact: true })).toBeVisible();
	expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBe(0);

	// Drill down from the seeded zone row directly into bounded server-filtered /events!
	await page.setViewportSize({ width: 1440, height: 900 });
	const zoneRow = page.locator('tr', { hasText: fixture.zone }).first();
	await zoneRow.getByRole('link', { name: 'Open', exact: true }).click();
	await expect(page).toHaveURL(new RegExp(`/events\\?.*zone=${fixture.zone}`));
	await expect(page.locator('[data-event-card]')).toHaveCount(6, { timeout: 15_000 });

	// Changing the Event date after drill-down clears sticky startMs/endMs range bounds.
	await page.getByLabel('Event date').fill('2026-01-02');
	await expect(page).not.toHaveURL(/startMs=/);
	expect(errors).toEqual([]);
});
