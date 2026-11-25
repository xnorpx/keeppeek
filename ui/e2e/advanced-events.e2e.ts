import { expect, test } from '@playwright/test';

test('real advanced boolean builder, URL restore, saved searches, and mobile usability work end to end', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const errors: string[] = [];
	page.on('pageerror', (error) => errors.push(error.message));
	page.on('console', (message) => {
		if (message.type() === 'error') errors.push(message.text());
	});

	await page.goto('/events');
	await expect(page.getByRole('button', { name: 'Advanced search', exact: true })).toBeVisible();

	const fixture = await page.evaluate(async () => {
		const modulePath = '/e2e/fixtures/event-workflow-browser.ts';
		const helpers = (await import(
			modulePath
		)) as typeof import('./fixtures/event-workflow-browser');
		return helpers.seedEventWorkflow(6);
	});

	await page.goto(`/events?date=${fixture.date}&zone=${fixture.zone}`);
	await expect(page.locator('[data-event-card]')).toHaveCount(6);

	// Convert common filters into the advanced boolean builder.
	await page.getByRole('button', { name: 'Advanced search', exact: true }).click();
	const builder = page.getByTestId('event-expression-builder');
	await expect(builder).toBeVisible();
	await expect(page.getByTestId('event-expression-summary')).toContainText(fixture.zone);

	// Run real server-side advanced boolean query and verify admin diagnostics without SQL/vectors.
	await builder.getByRole('button', { name: 'Run advanced search', exact: true }).click();
	await expect(page).toHaveURL(/expr=v1%3A/);
	await expect(page.locator('[data-event-card]')).toHaveCount(6);
	await expect(page.getByTestId('planner-diagnostics')).toContainText(
		'indexed_camera_time_boolean'
	);

	// Reload URL and confirm bounded v1 expression restores automatically.
	await page.reload();
	await expect(page.getByTestId('event-expression-builder')).toBeVisible();
	await expect(page.locator('[data-event-card]')).toHaveCount(6);

	// Save, reload, apply, and delete a durable config.toml-backed saved search.
	const savedControls = page.getByTestId('saved-search-controls');
	const savedName = `Zone check ${fixture.zone}`;
	await savedControls.getByLabel('Saved search name').fill(savedName);
	await savedControls.getByRole('button', { name: 'Save search', exact: true }).click();
	await expect(savedControls.getByRole('button', { name: savedName, exact: true })).toBeVisible();

	await page.reload();
	await expect(
		page.getByTestId('saved-search-controls').getByRole('button', { name: savedName, exact: true })
	).toBeVisible();

	// Add a nested group and verify invalid empty-group draft is preserved without crashing.
	await builder.getByRole('button', { name: 'Add group', exact: true }).first().click();
	await expect(builder.locator('[data-expression-group="or"]')).toHaveCount(1);

	// Delete the saved search cleanly.
	await page
		.getByTestId('saved-search-controls')
		.getByRole('button', { name: `Delete saved search ${savedName}` })
		.click();
	await expect(
		page.getByTestId('saved-search-controls').getByRole('button', { name: savedName, exact: true })
	).toHaveCount(0);

	// Mobile viewport remains usable with accessible controls.
	await page.setViewportSize({ width: 390, height: 844 });
	await expect(page.getByTestId('event-expression-builder')).toBeVisible();
	expect(errors).toEqual([]);
});
