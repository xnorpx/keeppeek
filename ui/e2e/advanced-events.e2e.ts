import { expect, test, type Page } from '@playwright/test';
import { eventDate, mockEvents } from './fixtures/events';

async function seedWorkflowEvents(page: Page, count: number) {
	return page.evaluate(async (n) => {
		const modulePath = '/e2e/fixtures/event-workflow-browser.ts';
		const helpers = (await import(
			modulePath
		)) as typeof import('./fixtures/event-workflow-browser');
		return helpers.seedEventWorkflow(n);
	}, count);
}

async function exerciseCsvAndTimeRange(page: Page, zone: string) {
	const builder = page.getByTestId('event-expression-builder');
	await builder.getByRole('button', { name: 'Add condition', exact: true }).first().click();
	const csvInput = builder.getByLabel('event_types values').last();
	await csvInput.fill('');
	await csvInput.pressSequentially('person, vehicle');
	await expect(csvInput).toHaveValue('person, vehicle');
	await expect(page.getByTestId('event-expression-summary')).toContainText('person, vehicle');

	await builder.getByRole('button', { name: 'Add condition', exact: true }).first().click();
	await builder.getByLabel('Predicate field').last().selectOption('time_range');
	await expect(builder.getByLabel('UTC start time')).toBeVisible();
	await expect(builder.getByLabel('UTC end time')).toBeVisible();
	await builder.getByRole('button', { name: 'Remove condition' }).last().click();
	await builder.getByRole('button', { name: 'Remove condition' }).last().click();
	await expect(page.getByTestId('event-expression-summary')).toContainText(zone);
}

async function exerciseSavedSearchAndHistory(page: Page, zone: string, date: string) {
	const savedControls = page.getByTestId('saved-search-controls');
	const savedName = `Zone check ${zone}`;
	await savedControls.getByLabel('Saved search name').fill(savedName);
	await savedControls.getByRole('button', { name: 'Save search', exact: true }).click();
	await expect(savedControls.getByRole('button', { name: savedName, exact: true })).toBeVisible();

	// Reload on clean /events and click the saved search to apply it end to end.
	await page.goto(`/events?date=${date}`);
	await page.getByRole('button', { name: 'Advanced search', exact: true }).click();
	await page
		.getByTestId('saved-search-controls')
		.getByRole('button', { name: savedName, exact: true })
		.click();
	await expect(page).toHaveURL(/saved=/);
	await expect(page.locator('[data-event-card]')).toHaveCount(6);

	// Browser Back restores the non-advanced state via popstate, Forward re-applies saved search.
	await page.goBack();
	await expect(page).not.toHaveURL(/saved=/);
	await page.goForward();
	await expect(page).toHaveURL(/saved=/);
	await expect(page.locator('[data-event-card]')).toHaveCount(6);
	return savedName;
}

async function exerciseNoResultsAndConflict(page: Page, savedName: string) {
	const builder = page.getByTestId('event-expression-builder');
	const zoneInput = builder.getByLabel('zones values').first();
	await zoneInput.fill('nonexistent-zone-xyz');
	await builder.getByRole('button', { name: 'Run advanced search', exact: true }).click();
	await expect(page.getByText('No events found.')).toBeVisible();

	// Trigger a real revision conflict by updating the saved search from a second browser tab.
	const secondPage = await page.context().newPage();
	await secondPage.goto(page.url());
	const secondSaved = secondPage.getByTestId('saved-search-controls');
	await expect(secondSaved.getByRole('button', { name: savedName, exact: true })).toBeVisible();
	await secondSaved.getByRole('button', { name: savedName, exact: true }).click();
	await secondSaved.getByRole('button', { name: 'Update rev 1', exact: true }).click();
	await expect(secondSaved.getByText('r2')).toBeVisible();
	await secondPage.close();

	// First tab still holds revision 1 and receives an explicit CAS conflict alert.
	await page
		.getByTestId('saved-search-controls')
		.getByRole('button', { name: 'Update rev 1', exact: true })
		.click();
	await expect(page.getByTestId('saved-search-error')).toContainText(/revision changed/i);
}

test('real advanced boolean builder, CSV/time_range, saved apply/conflict, popstate, no-results, and mobile work end to end', async ({
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
	const fixture = await seedWorkflowEvents(page, 6);

	await page.goto(`/events?date=${fixture.date}&zone=${fixture.zone}`);
	await expect(page.locator('[data-event-card]')).toHaveCount(6);
	await page.getByRole('button', { name: 'Advanced search', exact: true }).click();
	const builder = page.getByTestId('event-expression-builder');
	await expect(builder).toBeVisible();
	await exerciseCsvAndTimeRange(page, fixture.zone);

	await builder.getByRole('button', { name: 'Run advanced search', exact: true }).click();
	await expect(page).toHaveURL(/expr=v1%3A/);
	await expect(page.locator('[data-event-card]')).toHaveCount(6);
	await expect(page.getByTestId('planner-diagnostics')).toContainText(
		'indexed_camera_time_boolean'
	);

	const savedName = await exerciseSavedSearchAndHistory(page, fixture.zone, fixture.date);
	await exerciseNoResultsAndConflict(page, savedName);

	// Reload to pick up r2, verify nested group creation, delete saved search, and check mobile.
	await page.reload();
	await page
		.getByTestId('event-expression-builder')
		.getByRole('button', { name: 'Add group', exact: true })
		.first()
		.click();
	await expect(
		page.getByTestId('event-expression-builder').locator('[data-expression-group="or"]')
	).toHaveCount(1);
	await page
		.getByTestId('saved-search-controls')
		.getByRole('button', { name: `Delete saved search ${savedName}` })
		.click();
	await expect(
		page.getByTestId('saved-search-controls').getByRole('button', { name: savedName, exact: true })
	).toHaveCount(0);

	await page.setViewportSize({ width: 390, height: 844 });
	await expect(page.getByTestId('event-expression-builder')).toBeVisible();
	expect(errors).toEqual([]);
});

test('hides advanced search cleanly when capability is absent while keeping ordinary filters working', async ({
	page
}) => {
	await mockEvents(page, [], { capabilityIds: [] });
	await page.goto(`/events?date=${eventDate}`);
	await expect(page.locator('[data-event-card]')).toHaveCount(5);
	await expect(page.getByRole('button', { name: 'Advanced search', exact: true })).toHaveCount(0);
});

test('shows semantic fallback notice when semantic capability is absent while builder stays usable', async ({
	page
}) => {
	await mockEvents(page, [], { capabilityIds: ['keeppeek.event-search.advanced.v1'] });
	await page.goto(`/events?date=${eventDate}`);
	await expect(page.locator('[data-event-card]')).toHaveCount(5);
	await page.getByRole('button', { name: 'Advanced search', exact: true }).click();
	await expect(page.getByTestId('semantic-fallback-notice')).toBeVisible();
});
