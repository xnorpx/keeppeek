import { expect, test, type BrowserContext, type Locator, type Page } from '@playwright/test';
import {
	cardAdminKey,
	cardSourceIds,
	reviewSourceIds,
	startHomeAssistantServer
} from './fixtures/home-assistant-server';
import { recordedAudioBackendRoute } from './fixtures/recorded-audio-playback';
import type { RecordingCoverageResponse } from '../src/lib/types';

async function comparison(
	page: Page,
	context: BrowserContext,
	baseURL: string | undefined,
	ids: readonly string[]
) {
	if (!baseURL) throw new Error('The recorded comparison requires a UI origin.');
	const origin = new URL(baseURL).origin;
	const recorder = await startHomeAssistantServer(origin, {
		trustedLocal: true,
		reviewFixture: 'four'
	});
	const redirect = recordedAudioBackendRoute(origin, recorder.url);
	async function close() {
		try {
			await context.unroute('**/*', redirect);
		} finally {
			await recorder.close();
		}
	}
	try {
		await context.route('**/*', redirect);
		const response = await page.request.get(recorder.url + '/recording-coverage?stream=sub', {
			headers: { Authorization: 'Bearer ' + cardAdminKey }
		});
		expect(response.ok()).toBe(true);
		const coverage: RecordingCoverageResponse = await response.json();
		const starts = ids.map(
			(id) =>
				coverage.cameras
					.find((camera) => camera.camera_id === id)
					?.streams.find((stream) => stream.stream_id === 'sub')?.ranges[0]?.start_ms ?? Infinity
		);
		const at = Math.max(...starts) + 2_000;
		expect(Number.isFinite(at)).toBe(true);
		const query = new URLSearchParams({
			camera: ids[0]!,
			stream: 'sub',
			at: String(at),
			audio: ids[0]!
		});
		for (const id of ids) query.append('compare', id);
		await page.goto('/keep?' + query);
		await expect(page.getByRole('heading', { name: 'Compare recordings' })).toBeVisible({
			timeout: 30_000
		});
	} catch (error) {
		await close();
		throw error;
	}
	return close;
}

async function tabTo(page: Page, target: Locator) {
	for (let i = 0; i < 80; i++) {
		if (await target.evaluate((element) => element === document.activeElement)) {
			expect(
				await target.evaluate(
					(element) =>
						element.matches(':focus-visible') &&
						parseFloat(getComputedStyle(element).outlineWidth) >= 2
				)
			).toBe(true);
			return;
		}
		await page.keyboard.press('Tab');
	}
	throw new Error('The comparison control could not be reached with Tab.');
}

test('keyboard selection keeps visible focus when adding and removing comparison cameras', async ({
	page,
	context,
	baseURL
}) => {
	test.setTimeout(90_000);
	const close = await comparison(page, context, baseURL, reviewSourceIds.slice(0, 3));
	try {
		const play = page.getByRole('button', { name: 'Play recordings' });
		await tabTo(page, play);
		await page.keyboard.press('Space');
		await expect(play).toHaveAttribute('aria-pressed', 'true');
		await page.keyboard.press('Space');
		await expect(play).toHaveAttribute('aria-pressed', 'false');
		const original = Number(new URL(page.url()).searchParams.get('at'));
		await page.keyboard.press('Tab');
		await expect(page.getByRole('button', { name: 'Forward five seconds' })).toBeFocused();
		await page.keyboard.press('Enter');
		await expect
			.poll(() => Number(new URL(page.url()).searchParams.get('at')))
			.toBe(original + 5000);
		const add = page.getByRole('combobox', { name: 'Add camera' });
		await tabTo(page, add);
		await page.keyboard.press('End');
		await expect(add).toHaveCount(0);
		const focus = page.getByRole('combobox', { name: 'Focused camera' });
		await expect(focus).toBeFocused();
		await page.keyboard.press('End');
		await expect(focus).toHaveValue(reviewSourceIds[3]);
		await page.keyboard.press('Tab');
		const audio = page.getByRole('combobox', { name: 'Audio source' });
		await expect(audio).toBeFocused();
		await page.keyboard.press('End');
		await expect(audio).toHaveValue(reviewSourceIds[3]);
		await tabTo(page, page.getByRole('button', { name: 'Remove card-camera-3' }));
		await page.keyboard.press('Enter');
		await expect(page.getByRole('article', { name: 'card-camera-3' })).toHaveCount(0);
		await expect(focus).toBeFocused();
		await expect(focus).toHaveValue(cardSourceIds[0]);
		await expect(audio).toHaveValue(cardSourceIds[0]);
	} finally {
		await close();
	}
});

test.describe('portrait comparison', () => {
	test.use({ hasTouch: true, isMobile: true });
	for (const width of [320, 390]) {
		test(
			'comparison controls stay within the screen and accept touch at ' + width + ' px',
			async ({ page, context, baseURL }, info) => {
				test.setTimeout(90_000);
				await page.setViewportSize({ width, height: 844 });
				await page.addInitScript(() =>
					Object.defineProperty(Navigator.prototype, 'hardwareConcurrency', {
						configurable: true,
						get: () => 2
					})
				);
				const close = await comparison(page, context, baseURL, cardSourceIds);
				try {
					const region = page
						.getByRole('heading', { name: 'Compare recordings' })
						.locator('xpath=ancestor::section[1]');
					const options = region.getByRole('button', { name: 'Comparison options' });
					await expect(options).toHaveAttribute('aria-expanded', 'false');
					await expect(region.locator('article video')).toHaveCount(1);
					const picture = await region.locator('article video').evaluate((video) => {
						const r = video.getBoundingClientRect();
						const nav = document.querySelector('[data-shell-mobile-nav]')?.getBoundingClientRect();
						return {
							top: r.top,
							bottom: r.bottom,
							height: r.height,
							navigationTop: nav?.top ?? innerHeight
						};
					});
					expect(picture.top).toBeGreaterThanOrEqual(0);
					expect(picture.height).toBeGreaterThan(100);
					expect(picture.bottom).toBeLessThanOrEqual(picture.navigationTop + 1);
					await info.attach('portrait-playing-area', {
						contentType: 'application/json',
						body: JSON.stringify(picture)
					});
					await info.attach('portrait-initial', {
						contentType: 'image/png',
						body: await page.screenshot()
					});
					await options.tap();
					await expect(options).toHaveAttribute('aria-expanded', 'true');
					const geometry = await region.evaluate((section) => ({
						body: document.documentElement.scrollWidth,
						viewport: window.innerWidth,
						section: section.scrollWidth,
						available: section.clientWidth,
						targets: Array.from(section.querySelectorAll<HTMLElement>('button,select,input'))
							.filter((el) => !el.hasAttribute('disabled') && el.getClientRects().length > 0)
							.map((el) => {
								const r = el.getBoundingClientRect();
								return {
									name: el.getAttribute('aria-label') || el.textContent?.trim(),
									width: Math.round(r.width),
									height: Math.round(r.height)
								};
							})
					}));
					await info.attach('portrait-control-geometry', {
						contentType: 'application/json',
						body: JSON.stringify(geometry)
					});
					await info.attach('portrait-comparison', {
						contentType: 'image/png',
						body: await page.screenshot()
					});
					expect(geometry.body).toBeLessThanOrEqual(width);
					expect(geometry.section).toBeLessThanOrEqual(geometry.available + 1);
					expect(geometry.targets.filter((t) => Math.min(t.width, t.height) < 44)).toEqual([]);
					const play = page.getByRole('button', { name: 'Play recordings' });
					await play.tap();
					await expect(play).toHaveAttribute('aria-pressed', 'true');
					await play.tap();
					await expect(play).toHaveAttribute('aria-pressed', 'false');
					await expect(region.locator('article video')).toHaveCount(1);
					await region
						.getByRole('combobox', { name: 'Focused camera' })
						.selectOption(cardSourceIds[1]);
					await expect(
						region.getByRole('article', { name: 'card-camera-1' }).locator('video')
					).toHaveCount(1);
					await region.getByRole('button', { name: 'Set range start' }).tap();
					await region.getByRole('button', { name: 'Forward five seconds' }).tap();
					await region.getByRole('button', { name: 'Set range end' }).tap();
					await expect(region.getByRole('button', { name: 'Export focused camera' })).toBeEnabled();
					await options.tap();
					await expect(options).toHaveAttribute('aria-expanded', 'false');
					await expect(region.getByRole('combobox', { name: 'Focused camera' })).toHaveCount(0);
					await expect(
						region.getByRole('article', { name: 'card-camera-1' }).locator('video')
					).toHaveCount(1);
				} finally {
					await close();
				}
			}
		);
	}
});
