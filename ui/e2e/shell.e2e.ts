import { expect, test, type Page } from '@playwright/test';
import { mockControlPeer } from './fixtures/control-peer';

async function mockEmptyRecorder(page: Page): Promise<void> {
	await mockControlPeer(page);
}

test('uses the fixed Paper desktop shell geometry', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await mockEmptyRecorder(page);
	await page.goto('/');

	const rail = page.getByRole('complementary', { name: 'Desktop navigation' });
	const primaryNavigation = page.getByRole('navigation', { name: 'Primary navigation' });
	const contextBar = page.locator('[data-shell-context]');
	const statusBar = page.locator('[data-shell-status]');

	await expect(rail).toBeVisible();
	await expect(rail).toHaveCSS('width', '64px');
	await expect(contextBar).toHaveCSS('height', '52px');
	await expect(statusBar).toBeVisible();
	await expect(statusBar).toHaveCSS('height', '32px');
	await expect(page.locator('[data-shell-mobile-nav]')).toBeHidden();
	await expect(primaryNavigation.getByRole('link')).toHaveCount(6);
	expect(
		await primaryNavigation
			.getByRole('link')
			.evaluateAll((links) => links.map((link) => link.getAttribute('aria-label')))
	).toEqual(['Peek', 'Keep', 'Events', 'Cameras', 'Health', 'Settings']);
	await expect(page.getByRole('link', { name: 'Peek', exact: true }).first()).toHaveAttribute(
		'aria-current',
		'page'
	);
});

test('uses the five-destination Paper mobile shell without horizontal overflow', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockEmptyRecorder(page);
	await page.goto('/');

	const mobileNavigation = page.locator('[data-shell-mobile-nav]');
	const links = mobileNavigation.getByRole('link');

	await expect(page.getByRole('complementary', { name: 'Desktop navigation' })).toBeHidden();
	await expect(mobileNavigation).toBeVisible();
	await expect(mobileNavigation).toHaveCSS('height', '78px');
	await expect(links).toHaveCount(5);
	expect((await links.allTextContents()).map((label) => label.trim())).toEqual([
		'Peek',
		'Keep',
		'Events',
		'Health',
		'More'
	]);
	await expect(mobileNavigation.getByRole('link', { name: 'Peek', exact: true })).toHaveAttribute(
		'aria-current',
		'page'
	);
	await expect(page.locator('[data-shell-context]')).toHaveCSS('height', '50px');
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
});
