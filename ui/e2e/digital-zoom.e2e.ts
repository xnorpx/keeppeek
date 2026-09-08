import { expect, test, type Locator, type Page } from '@playwright/test';
import { eventDate, mockEvents } from './fixtures/events';
import { keepModeDate, mockKeepModes } from './fixtures/keep-modes';
import { mockMixedHealth } from './fixtures/peek';

async function expectScale(scope: Locator, scale: number) {
	await expect
		.poll(async () =>
			Number(await scope.locator('[data-focused-media]').getAttribute('data-digital-zoom'))
		)
		.toBeCloseTo(scale, 2);
}

async function dragMedia(page: Page, scope: Locator) {
	const bounds = await scope
		.getByRole('application', { name: 'Digital zoom viewport' })
		.boundingBox();
	expect(bounds).not.toBeNull();
	await page.mouse.move(bounds!.x + bounds!.width / 2, bounds!.y + bounds!.height / 2);
	await page.mouse.down();
	await page.mouse.move(bounds!.x + bounds!.width / 2 + 45, bounds!.y + bounds!.height / 2 + 30, {
		steps: 6
	});
	await page.mouse.up();
}

test('focused live supports wheel and drag without zooming wall tiles, and resets on camera change', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await mockMixedHealth(page);
	await page.goto('/viewer?camera=front-door');
	let focus = page.getByRole('region', { name: 'Front Door focus', exact: true });
	await expect(focus.locator('[data-focused-media]')).toBeVisible();
	await expect(page.locator('[data-peek-wall] [data-focused-media]')).toHaveCount(0);
	await expect(focus.getByLabel('Camera filmstrip').locator('[data-focused-media]')).toHaveCount(0);
	const video = await focus.locator('[data-peek-focus-stage] video').elementHandle();
	const viewport = focus.getByRole('application', { name: 'Digital zoom viewport' });
	await viewport.hover();
	await page.mouse.wheel(0, -100);
	await expectScale(focus, 1);
	await page.keyboard.down('Alt');
	await page.mouse.wheel(0, -100);
	await page.keyboard.up('Alt');
	await expect
		.poll(async () =>
			Number(await focus.locator('[data-focused-media]').getAttribute('data-digital-zoom'))
		)
		.toBeGreaterThan(1);
	await dragMedia(page, focus);
	expect(await video!.evaluate((element) => element.isConnected)).toBe(true);
	await focus.getByRole('button', { name: 'Reset digital zoom', exact: true }).click();
	await expectScale(focus, 1);
	await viewport.dblclick();
	await expectScale(focus, 2);
	await focus.getByRole('button', { name: 'Focus Porch', exact: true }).click();
	focus = page.getByRole('region', { name: 'Porch focus', exact: true });
	await expectScale(focus, 1);
});

test('focused zoom stays top-left outside the picture and clear of other controls', async ({
	page
}, testInfo) => {
	await mockMixedHealth(page);
	await page.goto('/viewer?camera=front-door');
	const focus = page.getByRole('region', { name: 'Front Door focus', exact: true });
	await focus.getByRole('button', { name: 'Digital zoom in', exact: true }).click();
	await expectScale(focus, 2);
	for (const size of [
		{ width: 1440, height: 900 },
		{ width: 1024, height: 576 },
		{ width: 768, height: 1024 },
		{ width: 900, height: 420 },
		{ width: 320, height: 740 }
	]) {
		await page.setViewportSize(size);
		await expect
			.poll(() =>
				focus.evaluate((element) => {
					const stage = element.querySelector('[data-peek-focus-stage]')!.getBoundingClientRect();
					const zoom = element
						.querySelector('[data-digital-zoom-controls]')!
						.getBoundingClientRect();
					const media = element.querySelector('[role="application"]')!.getBoundingClientRect();
					const quality = element.querySelector('.focus-controls')!.getBoundingClientRect();
					const information = element
						.querySelector('[data-live-video-camera-controls]')!
						.getBoundingClientRect();
					const qualityGroup = element
						.querySelector('.focus-quality-options')!
						.getBoundingClientRect();
					const overlaps = (bounds: DOMRect) =>
						zoom.left < bounds.right &&
						zoom.right > bounds.left &&
						zoom.top < bounds.bottom &&
						zoom.bottom > bounds.top;
					return {
						topLeft:
							zoom.top >= stage.top && zoom.top - stage.top <= 12 && zoom.left - stage.left <= 12,
						outsidePicture: zoom.bottom <= media.top,
						mediaVisible: media.width > 0 && media.height > 0,
						clearOfControls: !overlaps(quality) && !overlaps(information),
						sameRow:
							Math.abs(zoom.top - qualityGroup.top) < 1 && Math.abs(zoom.top - information.top) < 1,
						sameHeight:
							zoom.height === 32 &&
							zoom.height === qualityGroup.height &&
							zoom.height === information.height
					};
				})
			)
			.toEqual({
				topLeft: true,
				outsidePicture: true,
				mediaVisible: true,
				clearOfControls: true,
				sameRow: true,
				sameHeight: true
			});
		await expect(
			focus.getByRole('button', { name: 'Reset digital zoom', exact: true })
		).toBeInViewport();
		await page.screenshot({ path: testInfo.outputPath(`digital-zoom-top-left-${size.width}.png`) });
	}
});

test('recorded zoom preserves transport, media identity, and fullscreen without arrow-key seeking', async ({
	page
}) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const requests = await mockKeepModes(page);
	await page.goto(`/keep?camera=front-door&stream=main&date=${keepModeDate}`);
	const player = page.locator('[data-keep-player]');
	await expect(player.locator('video')).toBeVisible();
	await player.getByRole('button', { name: 'Pause recording', exact: true }).click();
	const mute = player.getByRole('button', { name: /^(Unmute|Mute) recording$/ });
	const wasMuted = (await mute.getAttribute('aria-label')) === 'Unmute recording';
	await mute.focus();
	await page.keyboard.press('Space');
	await expect(mute).toHaveAttribute(
		'aria-label',
		wasMuted ? 'Mute recording' : 'Unmute recording'
	);
	await expect(player).toHaveAttribute('data-keyboard-playing', 'false');
	const video = await player.locator('video').elementHandle();
	const opened = requests.storedOpens.length;
	const playhead = await player.getAttribute('data-recording-playhead-ms');
	await player.getByRole('button', { name: 'Digital zoom in', exact: true }).click();
	await page.keyboard.press('ArrowRight');
	await expect(player).toHaveAttribute('data-recording-playhead-ms', playhead!);
	await expectScale(player, 2);
	await player.getByRole('button', { name: 'Play recording', exact: true }).click();
	await player.getByRole('button', { name: 'Pause recording', exact: true }).click();
	await expectScale(player, 2);
	expect(requests.storedOpens).toHaveLength(opened);
	expect(await video!.evaluate((element) => element.isConnected)).toBe(true);
	await player.getByRole('button', { name: 'Enter recording fullscreen', exact: true }).click();
	await expect
		.poll(() => page.evaluate(() => document.fullscreenElement?.hasAttribute('data-keep-player')))
		.toBe(true);
	await expect(player.getByRole('button', { name: 'Play recording', exact: true })).toBeVisible();
	await expectScale(player, 2);
	await player.getByRole('button', { name: 'Exit recording fullscreen', exact: true }).click();
	await expect.poll(() => page.evaluate(() => document.fullscreenElement === null)).toBe(true);
	await player.getByRole('button', { name: 'Reset digital zoom', exact: true }).click();
	await expectScale(player, 1);
});

test('event-image inspection leaves the source and evidence links unchanged and resets before closing', async ({
	page
}) => {
	await page.setViewportSize({ width: 1024, height: 768 });
	const requests = await mockEvents(page);
	await page.goto(`/events?date=${eventDate}`);
	await page.locator('[data-event-card="front-door:person-high"]').click();
	const detail = page.getByRole('complementary', { name: 'Event detail' });
	const image = detail.locator('[data-event-preview-image]');
	await expect(image).toBeVisible();
	const source = await image.getAttribute('src');
	const evidence = await detail
		.getByRole('link', { name: 'Open at this moment', exact: true })
		.getAttribute('href');
	const fetched = requests.eventMediaFetches.length;
	await detail.getByRole('button', { name: 'Digital zoom in', exact: true }).click();
	await dragMedia(page, detail);
	await expectScale(detail, 2);
	await expect(image).toHaveAttribute('src', source!);
	await expect(
		detail.getByRole('link', { name: 'Open at this moment', exact: true })
	).toHaveAttribute('href', evidence!);
	expect(requests.eventMediaFetches).toHaveLength(fetched);
	await page.keyboard.press('Escape');
	await expect(detail).toBeVisible();
	await expectScale(detail, 1);
	await page.keyboard.press('Escape');
	await expect(detail).toBeHidden();
});

async function seekRecordingStart(player: Locator) {
	const position = player.getByRole('slider', { name: 'Recording position', exact: true });
	await expect(position).toBeEnabled();
	const initialMs = Number(await player.getAttribute('data-recording-playhead-ms'));
	await position.press('ArrowRight');
	await expect
		.poll(async () => Number(await player.getAttribute('data-recording-playhead-ms')))
		.toBeGreaterThan(initialMs);
	const advancedMs = Number(await player.getAttribute('data-recording-playhead-ms'));
	await position.press('Home');
	await expect
		.poll(async () => Number(await player.getAttribute('data-recording-playhead-ms')))
		.toBeLessThan(advancedMs);
	await expect(position).toHaveValue('0');
}

test('real recorded pixels remain decoded through zoom, pause, seek, and viewport resize', async ({
	page
}, testInfo) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await page.goto('/keep?stream=main');
	const player = page.getByRole('region', { name: 'Recorded video player' });
	const video = player.locator('video');
	await expect
		.poll(() =>
			video.evaluate((media: HTMLVideoElement) => media.readyState >= 2 && media.videoWidth > 0)
		)
		.toBe(true);
	await player.getByRole('combobox', { name: 'Playback speed' }).selectOption('0.25');
	await player.getByRole('button', { name: 'Pause recording', exact: true }).click();
	const original = await video.elementHandle();
	const source = await video.getAttribute('src');
	const colors = await video.evaluate((media: HTMLVideoElement) => {
		const canvas = document.createElement('canvas');
		canvas.width = 64;
		canvas.height = 36;
		const context = canvas.getContext('2d')!;
		context.drawImage(media, 0, 0, 64, 36);
		const pixels = context.getImageData(0, 0, 64, 36).data;
		const distinct = new Set<number>();
		for (let offset = 0; offset < pixels.length; offset += 4) {
			distinct.add((pixels[offset] << 16) | (pixels[offset + 1] << 8) | pixels[offset + 2]);
		}
		return distinct.size;
	});
	expect(colors).toBeGreaterThan(16);
	await player.getByRole('button', { name: 'Digital zoom in', exact: true }).click();
	await dragMedia(page, player);
	await expectScale(player, 2);
	await expect(video).toHaveJSProperty('paused', true);
	await seekRecordingStart(player);
	await expectScale(player, 2);
	await player.getByRole('button', { name: 'Play recording', exact: true }).click();
	await expect(video).toHaveJSProperty('paused', false);
	await player.getByRole('button', { name: 'Pause recording', exact: true }).click();
	await expectScale(player, 2);
	await expect(video).toHaveAttribute('src', source!);
	expect(await original!.evaluate((element) => element.isConnected)).toBe(true);
	for (const width of [1440, 1024, 768, 320]) {
		await page.setViewportSize({ width, height: 900 });
		await expect(
			player.getByRole('button', { name: 'Play recording', exact: true })
		).toBeInViewport();
		await expect(
			player.getByRole('button', { name: 'Reset digital zoom', exact: true })
		).toBeInViewport();
		await expect
			.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
			.toBe(true);
		await page.screenshot({ path: testInfo.outputPath(`digital-zoom-recorded-${width}.png`) });
	}
	await player.getByRole('button', { name: 'Reset digital zoom', exact: true }).click();
	await expectScale(player, 1);
});

test.describe('touch live toolbar', () => {
	test.use({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
	test('keeps one aligned row with full-size touch targets and scrollable access to every control', async ({
		page
	}) => {
		await mockMixedHealth(page);
		await page.goto('/viewer?camera=front-door');
		const focus = page.getByRole('region', { name: 'Front Door focus', exact: true });
		const zoomIn = focus.getByRole('button', { name: 'Digital zoom in', exact: true });
		await zoomIn.tap();
		await expectScale(focus, 2);
		const target = await zoomIn.boundingBox();
		expect(target!.width).toBeGreaterThanOrEqual(44);
		expect(target!.height).toBeGreaterThanOrEqual(44);
		const heights = await focus.evaluate((element) =>
			[
				'[data-digital-zoom-controls]',
				'.focus-quality-options',
				'[data-live-video-camera-controls]'
			].map((selector) => {
				const bounds = element.querySelector(selector)!.getBoundingClientRect();
				return { top: bounds.top, height: bounds.height };
			})
		);
		expect(heights[0].height).toBe(48);
		expect(heights[1]).toEqual(heights[0]);
		expect(heights[2]).toEqual(heights[0]);
		const information = focus.getByRole('button', { name: 'Front Door camera information' });
		await information.scrollIntoViewIfNeeded();
		await expect(information).toBeInViewport();
		await focus.getByRole('button', { name: 'Reset digital zoom', exact: true }).tap();
		await expectScale(focus, 1);
		await expect
			.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
			.toBe(true);
	});
});

for (const viewportSize of [
	{ width: 320, height: 740 },
	{ width: 390, height: 844 },
	{ width: 768, height: 1024 }
]) {
	test.describe(`touch viewport ${viewportSize.width}`, () => {
		test.use({ viewport: viewportSize, isMobile: true, hasTouch: true });
		test('pinches and pans with two real touch pointers, resets, and restores ordinary page behavior', async ({
			page,
			context
		}, testInfo) => {
			await mockEvents(page);
			await page.goto(`/events?date=${eventDate}`);
			await page.locator('[data-event-card="front-door:person-high"]').tap();
			const detail = page.getByRole('complementary', { name: 'Event detail' });
			await expect(detail.locator('[data-event-preview-image]')).toBeVisible();
			const viewport = detail.getByRole('application', { name: 'Digital zoom viewport' });
			const bounds = await viewport.boundingBox();
			expect(bounds).not.toBeNull();
			const centerX = bounds!.x + bounds!.width / 2;
			const centerY = bounds!.y + bounds!.height / 2;
			const session = await context.newCDPSession(page);
			await session.send('Input.dispatchTouchEvent', {
				type: 'touchStart',
				touchPoints: [
					{ id: 1, x: centerX - 25, y: centerY },
					{ id: 2, x: centerX + 25, y: centerY }
				]
			});
			await session.send('Input.dispatchTouchEvent', {
				type: 'touchMove',
				touchPoints: [
					{ id: 1, x: centerX - 50, y: centerY + 15 },
					{ id: 2, x: centerX + 50, y: centerY + 15 }
				]
			});
			await session.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
			await expectScale(detail, 2);
			await page.screenshot({ path: testInfo.outputPath('digital-zoom-touch.png') });
			await detail.getByRole('button', { name: 'Reset digital zoom', exact: true }).tap();
			await expectScale(detail, 1);
			await expect
				.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
				.toBe(true);
			await detail.getByRole('button', { name: 'Close event detail', exact: true }).tap();
			await expect(detail).toBeHidden();
			expect(await page.evaluate(() => getComputedStyle(document.body).touchAction)).toBe('auto');
			await session.detach();
		});
	});
}
