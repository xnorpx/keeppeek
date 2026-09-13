import { expect, test } from '@playwright/test';
import { writeFile } from 'node:fs/promises';
import {
	mockKeepModes,
	keepModeDate,
	keepModeCameras,
	keepModeDayStartMs,
	keepModeOlderDate
} from './fixtures/keep-modes';
import { mockControlPeer } from './fixtures/control-peer';
import { mixedCameras, mixedHealth } from './fixtures/peek';

test.describe('touch input', () => {
	test.use({ hasTouch: true, isMobile: true });
	test('mobile onboarding cancel has a 44px touch target', async ({ page }) => {
		await page.setViewportSize({ width: 390, height: 844 });
		await mockControlPeer(page, { cameras: mixedCameras, health: mixedHealth });
		await page.goto('/cameras/new');
		const cancel = page.getByRole('button', { name: 'Cancel add camera' });
		await expect(cancel).toBeVisible();
		const bounds = await cancel.boundingBox();
		expect(bounds).not.toBeNull();
		expect(Math.min(bounds!.width, bounds!.height)).toBeGreaterThanOrEqual(44);
		await cancel.tap();
		await expect(page).toHaveURL(/\/cameras$/);
	});

	for (const width of [320, 390]) {
		test(`mobile Keep shows recording history above navigation without scrolling at ${width}px`, async ({
			page
		}, testInfo) => {
			await page.setViewportSize({ width, height: 844 });
			await mockKeepModes(page);
			await page.goto(`/keep?camera=front-door&date=${keepModeDate}`);
			await expect(page.locator('[data-keyboard-ready]')).toHaveAttribute(
				'data-keyboard-ready',
				'true'
			);
			const timeline = page.getByRole('region', { name: 'Recording timeline', exact: true });
			const navigation = page.locator('[data-shell-mobile-nav]');
			const eventCard = timeline.getByRole('button', { name: / event at / }).first();
			await expect(timeline).toHaveAttribute('data-timeline-orientation', 'horizontal');
			await expect(timeline).toHaveAttribute('aria-busy', 'false');
			await expect(eventCard).toBeVisible();
			await expect(navigation).toBeVisible();
			await page.evaluate(() => document.fonts.ready.then(() => undefined));
			const [timelineBounds, eventBounds, navigationBounds] = await Promise.all([
				timeline.boundingBox(),
				eventCard.boundingBox(),
				navigation.boundingBox()
			]);
			if (!timelineBounds || !eventBounds || !navigationBounds)
				throw new Error('Initial Keep history geometry is unavailable');
			const scroll = await page.evaluate(() => ({
				page: window.scrollY,
				main: document.querySelector('[data-shell-main]')?.scrollTop,
				content: document.querySelector('[data-keep-view-content]')?.scrollTop
			}));
			await testInfo.attach('initial-keep-geometry', {
				body: JSON.stringify({ timelineBounds, eventBounds, navigationBounds, scroll }, null, 2),
				contentType: 'application/json'
			});
			const screenshotPath = testInfo.outputPath(`initial-keep-${width}.png`);
			await page.screenshot({ path: screenshotPath, fullPage: false });
			await testInfo.attach('initial-keep-viewport', {
				path: screenshotPath,
				contentType: 'image/png'
			});
			expect(
				await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)
			).toBe(true);
			expect(scroll, 'the initial page and content have not scrolled').toEqual({
				page: 0,
				main: 0,
				content: 0
			});
			expect(
				eventBounds.y + eventBounds.height,
				'a complete recording event must appear above the fixed bottom navigation'
			).toBeLessThanOrEqual(navigationBounds.y);
			await expect(eventCard).toBeInViewport({ ratio: 1 });
		});
	}

	test('mobile Keep shows healthy decoded footage and recording history together', async ({
		page
	}, testInfo) => {
		await page.setViewportSize({ width: 390, height: 844 });
		await page.goto('/keep?stream=main');
		const player = page.getByRole('region', { name: 'Recorded video player' });
		const video = player.locator('video');
		await expect
			.poll(
				() =>
					video.evaluate(
						(media: HTMLVideoElement) =>
							media.readyState >= 2 &&
							media.videoWidth > 0 &&
							media.getVideoPlaybackQuality().totalVideoFrames > 0
					),
				{ timeout: 15_000 }
			)
			.toBe(true);
		await expect(player.getByRole('alert')).toHaveCount(0);
		const timeline = page.getByRole('region', { name: 'Recording timeline', exact: true });
		await expect(timeline).toHaveAttribute('aria-busy', 'false');
		await expect(timeline.getByRole('button', { name: /^Footage / }).first()).toBeVisible();
		const timelineBounds = await timeline.boundingBox();
		const navigationBounds = await page.locator('[data-shell-mobile-nav]').boundingBox();
		if (!timelineBounds || !navigationBounds)
			throw new Error('Healthy Keep history geometry is unavailable');
		await page.screenshot({ path: testInfo.outputPath('healthy-mobile-keep.png') });
		expect(
			timelineBounds.y + timelineBounds.height,
			'recording history remains above navigation while video decodes'
		).toBeLessThanOrEqual(navigationBounds.y);
		await expect(timeline).toBeInViewport({ ratio: 1 });
		await expect(player.getByRole('slider', { name: 'Recording position' })).toBeInViewport({
			ratio: 1
		});
	});

	test('mobile playback options preserve the player, all rates, volume, and focus', async ({
		page
	}) => {
		await page.setViewportSize({ width: 390, height: 844 });
		const requests = await mockKeepModes(page);
		await page.goto(`/keep?camera=front-door&date=${keepModeDate}`);
		const player = page.locator('[data-keep-player]');
		const video = player.locator('video');
		await expect(video).toBeVisible();
		await video.dispatchEvent('loadeddata');
		const identity = await video.elementHandle();
		const source = await video.getAttribute('src');
		const counters = () => [
			requests.storedOpens.length,
			requests.storedCloses.length,
			requests.storedSeeks.length
		];
		const before = counters();
		const trigger = page.getByRole('button', { name: 'Playback options', exact: true });
		await trigger.tap();
		const sheet = page.getByRole('dialog', { name: 'Playback options', exact: true });
		await expect(sheet).toBeVisible();
		const rates = ['0.25', '0.5', '1', '1.5', '2', '4', '8'];
		await expect(
			sheet.getByRole('combobox', { name: 'Playback speed' }).locator('option')
		).toHaveText(rates.map((rate) => `${rate}x`));
		for (const rate of rates) {
			await sheet.getByRole('combobox', { name: 'Playback speed' }).selectOption(rate);
			await expect(video).toHaveJSProperty('playbackRate', Number(rate));
		}
		await sheet.getByRole('slider', { name: 'Recording volume' }).press('Home');
		await expect(video).toHaveJSProperty('volume', 0);
		await sheet.getByRole('slider', { name: 'Recording volume' }).press('End');
		await expect(video).toHaveJSProperty('volume', 1);
		await page.keyboard.press('Escape');
		await expect(sheet).toBeHidden();
		await expect(trigger).toBeFocused();
		expect(await identity!.evaluate((element) => element.isConnected)).toBe(true);
		await expect(video).toHaveAttribute('src', source!);
		expect(counters()).toEqual(before);
		const speed = player.getByRole('button', { name: 'Playback speed 8x, open playback options' });
		await speed.tap();
		await expect(sheet).toBeVisible();
		await sheet.getByRole('button', { name: 'Done', exact: true }).tap();
		await expect(speed).toBeFocused();
	});

	test('mobile camera and day changes survive Back with wrapping navigation and search', async ({
		page
	}) => {
		await page.setViewportSize({ width: 390, height: 844 });
		await mockKeepModes(page);
		const moment = keepModeDayStartMs + 6 * 60 * 60_000 + 7 * 60_000;
		await page.goto(`/keep?camera=front-door&date=${keepModeDate}&at=${moment}`);
		const trigger = page.getByRole('button', { name: /^Camera and date,/ });
		await trigger.tap();
		const sheet = page.getByRole('dialog', { name: 'Camera and date', exact: true });
		await expect(
			sheet.getByRole('button', { name: 'Next recorded day', exact: true })
		).toBeDisabled();
		await expect(sheet.getByText('No later recorded day is available.')).toBeVisible();
		await sheet.getByRole('button', { name: 'Previous camera, Camera 10' }).tap();
		await expect(sheet.locator('[data-camera-switcher]')).toHaveAttribute(
			'data-selected-camera',
			'camera-10'
		);
		await expect(sheet.getByRole('button', { name: 'Next camera, Front Door' })).toBeDisabled();
		await page.locator('[data-keep-player] video').dispatchEvent('loadeddata');
		await sheet.getByRole('button', { name: 'Next camera, Front Door' }).tap();
		await page.locator('[data-keep-player] video').dispatchEvent('loadeddata');
		await sheet.getByRole('button', { name: /^Choose camera,/ }).tap();
		const search = page.getByRole('searchbox', { name: 'Find a Keep camera' });
		await search.fill('Camera 10');
		await page.getByRole('option', { name: /Camera 10/ }).tap();
		await page.locator('[data-keep-player] video').dispatchEvent('loadeddata');
		await sheet.getByRole('button', { name: 'Previous recorded day', exact: true }).tap();
		await expect(sheet.getByRole('combobox', { name: 'Recorded day' })).toHaveValue(
			keepModeOlderDate
		);
		await expect(page).toHaveURL(new RegExp(`date=${keepModeOlderDate}`));
		const selectedUrl = page.url();
		await page.goBack();
		await expect(sheet).toBeHidden();
		await expect(page).toHaveURL(selectedUrl);
		await expect(trigger).toBeFocused();
		await expect(page.locator('[data-keep-player]')).toHaveAttribute(
			'data-recording-playhead-ms',
			String(moment - 86_400_000)
		);
	});

	test('changing recorded day at playback midnight keeps the chosen UTC day', async ({ page }) => {
		await page.setViewportSize({ width: 390, height: 844 });
		const dayEndMs = keepModeDayStartMs + 86_400_000;
		const olderDayMs = Date.parse(`${keepModeOlderDate}T00:00:00Z`);
		await mockControlPeer(page, {
			cameras: keepModeCameras(1),
			storedRanges: [
				{ sourceId: 'front-door', streamId: 'main', startMs: dayEndMs - 60_000, endMs: dayEndMs },
				{
					sourceId: 'front-door',
					streamId: 'main',
					startMs: olderDayMs + 6 * 60 * 60_000,
					endMs: olderDayMs + 7 * 60 * 60_000
				}
			]
		});
		await page.addInitScript(() => {
			Object.defineProperty(HTMLMediaElement.prototype, 'play', {
				configurable: true,
				value() {
					return Promise.resolve();
				}
			});
		});
		await page.goto(`/keep?camera=front-door&date=${keepModeDate}&at=${dayEndMs - 1_000}`);
		const player = page.locator('[data-keep-player]');
		await expect(player).toHaveAttribute('data-recording-playhead-ms', String(dayEndMs - 1_000));
		await player.locator('video').dispatchEvent('loadeddata');
		await player.locator('video').evaluate((video) => {
			Object.defineProperty(video, 'currentTime', { configurable: true, value: 1 });
			video.dispatchEvent(new Event('timeupdate'));
			video.dispatchEvent(new Event('ended'));
		});
		await expect(player).toHaveAttribute('data-recording-playhead-ms', String(dayEndMs));
		await page.getByRole('button', { name: /^Camera and date,/ }).tap();
		await page.getByRole('button', { name: 'Previous recorded day', exact: true }).tap();
		await expect(player).toHaveAttribute('data-recording-playhead-ms', String(olderDayMs));
		await expect(page.getByRole('combobox', { name: 'Recorded day', exact: true })).toHaveValue(
			keepModeOlderDate
		);
		await page.getByRole('button', { name: 'Done', exact: true }).tap();
		await expect(
			player.getByText('No retained recording is available near this moment.', { exact: false })
		).toBeVisible();
		expect(new URL(page.url()).searchParams.get('at')).toBe(String(olderDayMs));
	});

	test('same-clock camera switches query new timeline events and recorded days', async ({
		page
	}, testInfo) => {
		await page.setViewportSize({ width: 390, height: 844 });
		const requests = await mockKeepModes(page, 2);
		const moment = keepModeDayStartMs + 6 * 60 * 60_000 + 7 * 60_000;
		await page.goto(`/keep?camera=front-door&date=${keepModeDate}&at=${moment}`);
		const video = page.locator('[data-keep-player] video');
		await video.dispatchEvent('loadeddata');
		await expect
			.poll(() =>
				requests.storedTimelineQueries.some(
					(query) =>
						query.sourceIds.includes('front-door') && query.availabilityBucketMs === 86_400_000
				)
			)
			.toBe(true);
		await page.getByRole('button', { name: /^Camera and date,/ }).tap();
		await page.getByRole('button', { name: 'Next camera, Camera 2' }).tap();
		await expect(page).toHaveURL(/camera=camera-2/);
		await video.dispatchEvent('loadeddata');
		try {
			await expect
				.poll(() => ({
					events: requests.eventSearchQueries.some((query) => query.sourceIds.includes('camera-2')),
					days: requests.storedTimelineQueries.some(
						(query) =>
							query.sourceIds.includes('camera-2') && query.availabilityBucketMs === 86_400_000
					)
				}))
				.toEqual({ events: true, days: true });
		} finally {
			const queryPath = testInfo.outputPath('camera-switch-queries.json');
			await writeFile(
				queryPath,
				JSON.stringify(
					{ timeline: requests.storedTimelineQueries, events: requests.eventSearchQueries },
					null,
					2
				)
			);
			await testInfo.attach('camera-switch-queries', {
				path: queryPath,
				contentType: 'application/json'
			});
		}
		await expect(page.locator('[data-keep-player]')).toHaveAttribute(
			'data-recording-playhead-ms',
			String(moment)
		);
		await expect(
			page.getByRole('button', { name: 'Previous recorded day', exact: true })
		).toBeEnabled();
	});

	test('mobile sheets trap focus and support Back and Forward without reopening media', async ({
		page
	}, testInfo) => {
		await page.setViewportSize({ width: 390, height: 844 });
		const requests = await mockKeepModes(page);
		await page.goto(`/keep?camera=front-door&date=${keepModeDate}`);
		const video = page.locator('[data-keep-player] video');
		await video.dispatchEvent('loadeddata');
		const identity = await video.elementHandle();
		const source = await video.getAttribute('src');
		const opened = requests.storedOpens.length;
		const trigger = page.getByRole('button', { name: 'Playback options', exact: true });
		await trigger.tap();
		const sheet = page.getByRole('dialog', { name: 'Playback options', exact: true });
		await expect(sheet).toBeVisible();
		await page.screenshot({
			path: testInfo.outputPath('mobile-playback-options.png'),
			animations: 'disabled'
		});
		for (let index = 0; index < 12; index += 1) {
			await page.keyboard.press(index % 3 === 0 ? 'Shift+Tab' : 'Tab');
			expect(await sheet.evaluate((element) => element.contains(document.activeElement))).toBe(
				true
			);
		}
		await page.goBack();
		await expect(sheet).toBeHidden();
		await expect(trigger).toBeFocused();
		await page.goForward();
		await expect(sheet).toBeVisible();
		const close = sheet.getByRole('button', { name: 'Close', exact: true });
		const bounds = await close.boundingBox();
		expect(Math.min(bounds!.width, bounds!.height)).toBeGreaterThanOrEqual(44);
		await close.tap();
		await expect(sheet).toBeHidden();
		await expect(trigger).toBeFocused();
		expect(await identity!.evaluate((element) => element.isConnected)).toBe(true);
		await expect(video).toHaveAttribute('src', source!);
		expect(requests.storedOpens).toHaveLength(opened);
		await page.getByRole('button', { name: /^Camera and date,/ }).tap();
		await expect(page.getByRole('dialog', { name: 'Camera and date', exact: true })).toBeVisible();
		await page.screenshot({
			path: testInfo.outputPath('mobile-camera-date.png'),
			animations: 'disabled'
		});
	});

	test('mobile camera switching keeps an unavailable clock explicit until a recording is chosen', async ({
		page
	}) => {
		await page.setViewportSize({ width: 390, height: 844 });
		await mockKeepModes(page);
		const moment = keepModeDayStartMs + 6 * 60 * 60_000 + 4 * 60_000;
		await page.goto(`/keep?camera=front-door&date=${keepModeDate}&at=${moment}`);
		await page.getByRole('button', { name: /^Camera and date,/ }).tap();
		await page.getByRole('button', { name: 'Previous camera, Camera 10' }).tap();
		await expect(page).toHaveURL(/camera=camera-10/);
		await page.getByRole('button', { name: 'Done', exact: true }).tap();
		const player = page.locator('[data-keep-player]');
		await expect(player).toHaveAttribute('data-recording-playhead-ms', String(moment));
		await expect(player).toHaveAttribute('data-camera-transition', 'idle');
		await expect(
			player.getByText('No recording covers this exact moment.', { exact: false })
		).toBeVisible();
		await player.getByRole('button', { name: 'Next recording', exact: true }).tap();
		await expect(player).toHaveAttribute(
			'data-recording-playhead-ms',
			String(keepModeDayStartMs + 6 * 60 * 60_000 + 400_000)
		);
		await expect(player.getByRole('button', { name: 'Next recording', exact: true })).toHaveCount(
			0
		);
	});

	test('an open mobile sheet survives landscape resize and restores visible focus', async ({
		page
	}, testInfo) => {
		await page.setViewportSize({ width: 390, height: 844 });
		const requests = await mockKeepModes(page);
		await page.goto(`/keep?camera=front-door&date=${keepModeDate}`);
		const video = page.locator('[data-keep-player] video');
		await video.dispatchEvent('loadeddata');
		const identity = await video.elementHandle();
		const source = await video.getAttribute('src');
		const counters = () => [
			requests.storedOpens.length,
			requests.storedCloses.length,
			requests.storedSeeks.length
		];
		const before = counters();
		await page.getByRole('button', { name: 'Playback options', exact: true }).tap();
		const sheet = page.getByRole('dialog', { name: 'Playback options', exact: true });
		await expect(sheet).toBeVisible();
		await page.setViewportSize({ width: 844, height: 390 });
		await expect(sheet).toBeVisible();
		await expect
			.poll(() =>
				sheet.evaluate((element) => {
					const bounds = element.getBoundingClientRect();
					return (
						bounds.y >= 0 &&
						Math.floor(bounds.bottom) <= window.innerHeight &&
						bounds.height <= window.innerHeight * 0.85 + 1
					);
				})
			)
			.toBe(true);
		await sheet.getByRole('combobox', { name: 'Playback speed', exact: true }).selectOption('2');
		await sheet.getByRole('button', { name: 'Done', exact: true }).tap();
		const timelineMode = page.getByRole('button', { name: 'Timeline', exact: true });
		await expect(timelineMode).toBeFocused();
		await expect(timelineMode).toBeInViewport({ ratio: 1 });
		await page.screenshot({
			path: testInfo.outputPath('landscape-keep.png'),
			animations: 'disabled'
		});
		await page.setViewportSize({ width: 390, height: 844 });
		await expect(sheet).toBeHidden();
		await page.getByRole('button', { name: 'Playback options', exact: true }).tap();
		await expect(sheet.getByRole('combobox', { name: 'Playback speed', exact: true })).toHaveValue(
			'2'
		);
		expect(await identity!.evaluate((element) => element.isConnected)).toBe(true);
		await expect(video).toHaveAttribute('src', source!);
		expect(counters()).toEqual(before);
	});

	test('mobile fullscreen retains inline playback rate and volume controls', async ({ page }) => {
		await page.setViewportSize({ width: 390, height: 844 });
		await mockKeepModes(page);
		await page.goto(`/keep?camera=front-door&date=${keepModeDate}`);
		const player = page.locator('[data-keep-player]');
		const video = player.locator('video');
		await video.dispatchEvent('loadeddata');
		const identity = await video.elementHandle();
		const source = await video.getAttribute('src');
		await player.getByRole('button', { name: 'Enter recording fullscreen', exact: true }).tap();
		await expect
			.poll(() => page.evaluate(() => document.fullscreenElement?.hasAttribute('data-keep-player')))
			.toBe(true);
		const rates = player.getByRole('combobox', { name: 'Playback speed', exact: true });
		await expect(rates.locator('option')).toHaveText([
			'0.25x',
			'0.5x',
			'1x',
			'1.5x',
			'2x',
			'4x',
			'8x'
		]);
		await rates.selectOption('2');
		await expect(video).toHaveJSProperty('playbackRate', 2);
		await player.getByRole('slider', { name: 'Recording volume', exact: true }).press('End');
		await expect(video).toHaveJSProperty('volume', 1);
		expect(await identity!.evaluate((element) => element.isConnected)).toBe(true);
		await expect(video).toHaveAttribute('src', source!);
		await player.getByRole('button', { name: 'Exit recording fullscreen', exact: true }).tap();
		await expect.poll(() => page.evaluate(() => document.fullscreenElement === null)).toBe(true);
		await expect(
			player.getByRole('button', { name: 'Playback speed 2x, open playback options' })
		).toBeVisible();
	});

	test('mobile playback options keep copy failure scoped and quality choices accessible', async ({
		page
	}) => {
		await page.setViewportSize({ width: 390, height: 844 });
		await mockKeepModes(page, 1);
		await page.addInitScript(() =>
			Object.defineProperty(navigator, 'clipboard', { configurable: true, value: undefined })
		);
		await page.goto(`/keep?camera=front-door&date=${keepModeDate}`);
		await page.getByRole('button', { name: 'Playback options', exact: true }).tap();
		const sheet = page.getByRole('dialog', { name: 'Playback options', exact: true });
		const quality = sheet.getByRole('combobox', { name: 'Quality', exact: true });
		await expect(quality.locator('option')).toHaveText([
			'Auto',
			'High',
			'Low',
			'Main exact',
			'Sub exact'
		]);
		await quality.selectOption('high');
		const copy = sheet.getByRole('button', { name: 'Copy link to this moment (sign-in required)' });
		await copy.tap();
		const copyDialog = page.getByRole('dialog', { name: 'Copy recording link', exact: true });
		await expect(copyDialog).toBeVisible();
		await expect(page.getByLabel('Authenticated recording link')).toBeFocused();
		await page.keyboard.press('Escape');
		await expect(copyDialog).toBeHidden();
		await expect(sheet).toBeVisible();
		await expect(copy).toBeFocused();
		await sheet.getByRole('button', { name: 'Refresh recordings' }).tap();
		await expect(quality).toHaveValue('high');
		await sheet.getByRole('button', { name: 'Done', exact: true }).tap();
		await expect(page.locator('[data-keep-player]')).toHaveAttribute(
			'data-recording-requested-variant',
			'high'
		);
	});
});
