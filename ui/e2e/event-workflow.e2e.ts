import { expect, test } from '@playwright/test';

test('real workflow persists explicit bulk scope and shares bookmarks across local workspaces', async ({
	page,
	browser
}, testInfo) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('console', (message) => {
		if (message.type() === 'warning' || message.type() === 'error') errors.push(message.text());
	});
	await page.goto('/events');
	await expect(page.getByRole('button', { name: 'Saved bookmarks', exact: true })).toBeVisible();
	const fixture = await page.evaluate(async () => {
		const modulePath = '/e2e/fixtures/event-workflow-browser.ts';
		const helpers = (await import(
			modulePath
		)) as typeof import('./fixtures/event-workflow-browser');
		return helpers.seedEventWorkflow(24);
	});
	const url = `/events?date=${fixture.date}&zone=${fixture.zone}`;
	await page.goto(url);
	await expect(page.locator('[data-event-card]')).toHaveCount(18);
	await expect(page.locator('[data-workflow-count]')).toHaveText('24 matching');
	await page.getByRole('button', { name: 'Mark 18 visible reviewed', exact: true }).click();
	await expect(
		page.getByRole('button', { name: 'Mark event unreviewed', exact: true })
	).toHaveCount(18);
	await page.getByLabel('Review filter', { exact: true }).selectOption('unreviewed');
	await expect(page.locator('[data-workflow-count]')).toHaveText('6 matching');
	await expect(page.locator('[data-event-card]')).toHaveCount(6);
	await page.getByRole('button', { name: 'Undo last review change', exact: true }).click();
	await expect(page.locator('[data-workflow-count]')).toHaveText('24 matching');
	await expect(page.locator('[data-event-card]')).toHaveCount(18);
	await page.locator('[data-workflow-select]').nth(0).check();
	await page.locator('[data-workflow-select]').nth(1).check();
	await page.getByRole('button', { name: 'Mark 2 selected reviewed', exact: true }).click();
	await expect(page.locator('[data-workflow-count]')).toHaveText('22 matching');
	await expect(
		page.getByRole('button', { name: 'Mark 2 selected reviewed', exact: true })
	).toBeVisible();
	await page.getByLabel('Review filter', { exact: true }).selectOption('all');
	await expect(page.locator('[data-workflow-count]')).toHaveText('24 matching');
	const firstKey = await page.locator('[data-event-card]').first().getAttribute('data-event-card');
	if (!firstKey) throw new Error('The first event identity is missing.');
	await page
		.locator(`[data-workflow-event="${firstKey}"]`)
		.getByRole('button', { name: 'Bookmark event', exact: true })
		.click();
	await page.locator(`[data-event-card="${firstKey}"]`).click();
	const detail = page.getByRole('complementary', { name: 'Event detail' });
	await detail.getByRole('button', { name: 'Edit bookmark note', exact: true }).click();
	await detail.getByRole('textbox', { name: 'Bookmark note' }).fill('Incident <door> & follow-up');
	await detail.getByRole('button', { name: 'Save bookmark note', exact: true }).click();
	await expect(detail.getByText('Incident <door> & follow-up', { exact: true })).toBeVisible();
	await expect(
		detail.getByText('Recording unavailable; metadata only', { exact: true })
	).toBeVisible();
	await page.reload();
	await expect(detail.getByText('Incident <door> & follow-up', { exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Close event detail', exact: true }).click();
	await page.getByLabel('Review filter', { exact: true }).selectOption('reviewed');
	await expect(page.locator('[data-workflow-count]')).toHaveText('2 matching');
	await expect(page).toHaveURL(/review=reviewed/);
	await page.locator('[data-workflow-select]').first().check();
	const secondContext = await browser.newContext({ baseURL: testInfo.project.use.baseURL });
	try {
		const second = await secondContext.newPage();
		await second.goto(`${url}&review=unreviewed&bookmarks=bookmarked`);
		await expect(second.locator('[data-workflow-count]')).toHaveText('1 matching');
		await expect(second.locator('[data-event-card]')).toHaveCount(1);
		await expect(
			second.getByRole('button', { name: 'Mark event reviewed', exact: true })
		).toHaveAttribute('aria-pressed', 'false');
	} finally {
		await secondContext.close();
	}
	await page.locator(`[data-event-card="${firstKey}"]`).click();
	await detail.getByRole('link', { name: 'Export event', exact: true }).click();
	await expect(page).toHaveURL(/mode=export/);
	await expect(page.locator('[data-export-source-bookmark]')).toBeVisible();
	await page.goBack();
	await expect(page).toHaveURL(/review=reviewed/);
	await expect(page.getByLabel('Review filter', { exact: true })).toHaveValue('reviewed');
	await expect(detail.getByText('Incident <door> & follow-up', { exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Close event detail', exact: true }).click();
	await expect(
		page.getByRole('button', { name: 'Mark 1 selected reviewed', exact: true })
	).toBeVisible();
	await page.getByRole('button', { name: 'Saved bookmarks', exact: true }).click();
	const library = page.getByRole('dialog', { name: 'Saved bookmarks', exact: true });
	await expect(library.locator(`[data-saved-bookmark="${firstKey}"]`)).toBeVisible();
	await expect(library.getByText('Incident <door> & follow-up', { exact: true })).toBeVisible();
	await library.getByRole('button', { name: 'Close saved bookmarks', exact: true }).click();
	const screenshot = testInfo.outputPath('event-workflow-desktop.png');
	await page.screenshot({ path: screenshot });
	await testInfo.attach('event-workflow-desktop.png', {
		path: screenshot,
		contentType: 'image/png'
	});
	expect(errors).toEqual([]);
});

test('real mobile keyboard actions retain note intent on a concurrent bookmark conflict', async ({
	page,
	context
}, testInfo) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	await page.goto('/events');
	await expect(page.getByRole('button', { name: 'Saved bookmarks', exact: true })).toBeVisible();
	const fixture = await page.evaluate(async () => {
		const modulePath = '/e2e/fixtures/event-workflow-browser.ts';
		return (
			(await import(modulePath)) as typeof import('./fixtures/event-workflow-browser')
		).seedEventWorkflow(2);
	});
	await page.goto(`/events?date=${fixture.date}&zone=${fixture.zone}`);
	await expect(page.locator('[data-event-card]')).toHaveCount(2);
	const key = await page.locator('[data-event-card]').first().getAttribute('data-event-card');
	if (!key) throw new Error('Event identity is missing');
	const actions = page.locator(`[data-workflow-event="${key}"]`);
	const reviewed = actions.getByRole('button', { name: 'Mark event reviewed', exact: true });
	await reviewed.focus();
	await page.keyboard.press('Space');
	await expect(
		actions.getByRole('button', { name: 'Mark event unreviewed', exact: true })
	).toHaveAttribute('aria-pressed', 'true');
	const bookmark = actions.getByRole('button', { name: 'Bookmark event', exact: true });
	await expect(bookmark).toBeEnabled();
	await bookmark.focus();
	await page.keyboard.press('Enter');
	await expect(actions.getByRole('button', { name: 'Remove bookmark', exact: true })).toBeEnabled();
	const bounds = await actions
		.getByRole('button', { name: 'Remove bookmark', exact: true })
		.boundingBox();
	expect(bounds?.height).toBeGreaterThanOrEqual(44);
	expect(bounds?.width).toBeGreaterThanOrEqual(44);
	await page.locator(`[data-event-card="${key}"]`).click();
	const detail = page.getByRole('complementary', { name: 'Event detail' });
	await detail.getByRole('button', { name: 'Edit bookmark note', exact: true }).click();
	await detail.getByRole('textbox', { name: 'Bookmark note' }).fill('My retained draft');
	const other = await context.newPage();
	try {
		await other.goto('/events');
		await expect(other.getByRole('button', { name: 'Saved bookmarks', exact: true })).toBeVisible();
		await other.evaluate(
			async ({ sourceId, eventId }) => {
				const modulePath = '/e2e/fixtures/event-workflow-browser.ts';
				const helper = (await import(
					modulePath
				)) as typeof import('./fixtures/event-workflow-browser');
				await helper.updateBookmarkNote({ sourceId, eventId }, 'Newer note from another tab');
			},
			{ sourceId: fixture.sourceId, eventId: decodeURIComponent(key.split(':')[1]!) }
		);
	} finally {
		await other.close();
	}
	await detail.getByRole('button', { name: 'Save bookmark note', exact: true }).click();
	await expect(detail.getByRole('alert')).toContainText('event workflow changed');
	await expect(detail.getByRole('textbox', { name: 'Bookmark note' })).toHaveValue(
		'My retained draft'
	);
	await detail.getByRole('button', { name: 'Reload and retry', exact: true }).click();
	await expect(detail.getByRole('alert')).toHaveCount(0);
	await detail.getByRole('button', { name: 'Cancel note editing', exact: true }).click();
	await expect(detail.getByText('My retained draft', { exact: true })).toBeVisible();
	await page.keyboard.press('Escape');
	await expect(detail).toHaveCount(0);
	await page
		.getByRole('button', { name: 'Filters', exact: false })
		.filter({ hasText: 'Filters' })
		.first()
		.click();
	await page.getByLabel('Review filter', { exact: true }).selectOption('dismissed');
	await page.getByLabel('Review filter', { exact: true }).selectOption('reviewed');
	await expect(page.locator('[data-workflow-count]')).toHaveText('1 matching');
	await page
		.getByRole('button', { name: /Filters/ })
		.first()
		.click();
	await page
		.getByRole('button', { name: 'Mark event unreviewed', exact: true })
		.scrollIntoViewIfNeeded();
	for (const width of [320, 768]) {
		await page.setViewportSize({ width, height: 844 });
		expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBe(0);
	}
	await page.setViewportSize({ width: 390, height: 844 });
	expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBe(0);
	const screenshot = testInfo.outputPath('event-workflow-mobile.png');
	await page.screenshot({ path: screenshot });
	await testInfo.attach('event-workflow-mobile.png', {
		path: screenshot,
		contentType: 'image/png'
	});
	expect(errors).toEqual([]);
});
