import { expect, test, type Page, type TestInfo } from '@playwright/test';
import { writeFile } from 'node:fs/promises';
import {
	keepModeCameras,
	keepModeDate,
	keepModeDayStartMs,
	mockKeepModes
} from './fixtures/keep-modes';
import { eventDate, mockEvents } from './fixtures/events';
import { mockControlPeer } from './fixtures/control-peer';

test.describe.configure({ mode: 'default' });

const momentMs = keepModeDayStartMs + 6 * 60 * 60_000 + 7 * 60_000 + 23_456;
const copyCommand = 'Copy link to this moment (sign-in required)';
type RecordingLinkWindow = typeof window & { recordingLinks: string[] };

async function attachScreenshot(page: Page, testInfo: TestInfo, name: string): Promise<void> {
	const screenshotPath = testInfo.outputPath(name);
	await page.screenshot({ path: screenshotPath });
	await testInfo.attach(name, { path: screenshotPath, contentType: 'image/png' });
}

async function capturedClipboard(page: Page): Promise<void> {
	await page.addInitScript(() => {
		const links: string[] = [];
		Object.assign(window, { recordingLinks: links });
		Object.defineProperty(navigator, 'clipboard', {
			configurable: true,
			value: {
				writeText: async (link: string) => {
					links.push(link);
				}
			}
		});
	});
}

async function copiedLink(page: Page): Promise<string> {
	return page.evaluate(() => (window as RecordingLinkWindow).recordingLinks.at(-1) ?? '');
}

async function playbackSnapshot(page: Page) {
	return page.evaluate(() => {
		const video = document.querySelector('video');
		const scroll = document.querySelector('[data-keep-view-content]');
		return {
			url: location.href,
			history: history.length,
			scroll: [scrollX, scrollY, scroll?.scrollTop, scroll?.scrollLeft],
			source: video?.getAttribute('src'),
			paused: video?.paused,
			clock: video?.currentTime
		};
	});
}

async function followMoment(page: Page, href: string): Promise<void> {
	await page.evaluate((targetHref) => {
		const anchor = document.createElement('a');
		anchor.href = targetHref;
		anchor.textContent = 'Another recording moment';
		document.body.append(anchor);
	}, href);
	await page.getByRole('link', { name: 'Another recording moment' }).click();
}

test('a moment link reconciles the date from its absolute timestamp', async ({ page }) => {
	await mockKeepModes(page, 1);
	await page.goto(`/keep?camera=front-door&stream=auto&date=2026-08-17&at=${momentMs}`);
	await expect(page.locator('[data-keep-player]')).toHaveAttribute(
		'data-recording-playhead-ms',
		String(momentMs)
	);
	await expect(page).toHaveURL(new RegExp(`date=${keepModeDate}`));
	await expect(
		page.getByRole('button', { name: 'Copy link to this moment (sign-in required)' })
	).toBeEnabled();
});

test('a gap link retains its exact time and offers explicit previous and next recordings', async ({
	page
}) => {
	const requests = await mockKeepModes(page, 1);
	const gapMs = keepModeDayStartMs + 6 * 60 * 60_000 + 12 * 60_000;
	await page.goto(`/keep?camera=front-door&stream=main&date=${keepModeDate}&at=${gapMs}`);
	await expect(
		page.getByRole('alert').filter({ hasText: 'No recording covers this exact moment' })
	).toBeVisible();
	await expect(page.locator('[data-keep-player]')).toHaveAttribute(
		'data-recording-playhead-ms',
		String(gapMs)
	);
	expect(requests.storedOpens).toHaveLength(0);
	await expect(page.getByRole('button', { name: 'Previous recording', exact: true })).toBeVisible();
	await page.getByRole('button', { name: 'Next recording', exact: true }).click();
	await expect(page.locator('[data-keep-player]')).toHaveAttribute(
		'data-recording-playhead-ms',
		String(keepModeDayStartMs + 6 * 60 * 60_000 + 15 * 60_000)
	);
});

test('a missing or unauthorized source never silently opens the first camera', async ({ page }) => {
	const requests = await mockKeepModes(page, 1);
	await page.goto(`/keep?camera=not-authorized&stream=main&at=${momentMs}`);
	await expect(
		page
			.getByRole('alert')
			.filter({ hasText: 'The requested camera is unavailable or you are not authorized' })
	).toBeVisible();
	expect(requests.storedOpens).toHaveLength(0);
	await expect(page.locator('video')).toHaveCount(0);
});

test('malformed moment parameters fail visibly without recording queries', async ({ page }) => {
	const requests = await mockKeepModes(page, 1);
	await page.goto('/keep?camera=front-door&date=2026-02-30&at=1e12');
	await expect(page.getByRole('alert').filter({ hasText: 'Invalid recording link' })).toBeVisible();
	expect(requests.storedTimelineQueries).toHaveLength(0);
	expect(requests.storedOpens).toHaveLength(0);
});

test('same-route links and browser back/forward restore each moment without reload', async ({
	page
}) => {
	const requests = await mockKeepModes(page, 2);
	await page.goto(`/keep?camera=front-door&stream=auto&at=${momentMs}`);
	const player = page.locator('[data-keep-player]');
	await expect(player).toHaveAttribute('data-recording-playhead-ms', String(momentMs));
	await page.evaluate(() => {
		document.body.dataset.navigationSentinel = 'retained';
	});
	const nextMomentMs = momentMs + 80_000;
	await followMoment(page, `/keep?camera=camera-2&stream=high&at=${nextMomentMs}`);
	await expect(player).toHaveAttribute('data-recording-playhead-ms', String(nextMomentMs));
	await expect(page.getByLabel('Quality')).toHaveValue('high');
	await expect.poll(() => requests.storedOpens.at(-1)?.sourceId).toBe('camera-2');
	await page.goBack();
	await expect(player).toHaveAttribute('data-recording-playhead-ms', String(momentMs));
	await expect(page.getByLabel('Quality')).toHaveValue('auto');
	await page.goForward();
	await expect(player).toHaveAttribute('data-recording-playhead-ms', String(nextMomentMs));
	await expect(page.locator('body')).toHaveAttribute('data-navigation-sentinel', 'retained');
	expect(requests.createAuthorizations).toHaveLength(1);
});

test('an unavailable requested stream is explained without changing the linked preference', async ({
	page
}) => {
	const requests = await mockKeepModes(page, 1);
	await page.goto(`/keep?camera=front-door&stream=sub&at=${momentMs}`);
	await expect(page.getByRole('status')).toContainText(
		'Sub has no indexed recording near this moment. Playing Main instead.'
	);
	await expect(page.getByLabel('Quality')).toHaveValue('sub');
	await expect(page).toHaveURL(/stream=sub/);
	await expect
		.poll(() => requests.storedOpens.map((request) => request.streamId))
		.toEqual(['main']);
});

test('absent retained footage stays explicit in every linked Keep mode', async ({ page }) => {
	const requests = await mockKeepModes(page, 1);
	const expiredMomentMs = Date.parse('2020-01-01T12:00:00Z');
	await page.goto(`/keep?camera=front-door&stream=main&mode=export&at=${expiredMomentMs}`);
	await expect(page.getByRole('alert')).toContainText(
		'No retained recording is available near this moment'
	);
	await expect(page).toHaveURL(new RegExp(`at=${expiredMomentMs}`));
	expect(requests.storedOpens).toHaveLength(0);
	await page.getByRole('button', { name: 'Timeline', exact: true }).click();
	await expect(page.getByRole('alert')).toContainText(
		'No retained recording is available near this moment'
	);
});

test('a decoder failure cannot move an exact link into a later fallback recording', async ({
	page
}) => {
	const cameras = keepModeCameras(1);
	cameras[0].profiles.push({
		name: 'Sub',
		stream: 'sub',
		encoding: 'h264',
		resolution: '320x180',
		framerate: 15
	});
	const requests = await mockControlPeer(page, {
		cameras,
		storedRanges: [
			{
				sourceId: 'front-door',
				streamId: 'main',
				startMs: momentMs - 60_000,
				endMs: momentMs + 60_000
			},
			{
				sourceId: 'front-door',
				streamId: 'sub',
				startMs: momentMs + 120_000,
				endMs: momentMs + 180_000
			}
		]
	});
	await page.goto(`/keep?camera=front-door&stream=main&at=${momentMs}`);
	await expect(page.locator('video')).toBeVisible();
	await page.locator('video').dispatchEvent('error', { bubbles: false });
	await expect(page.getByRole('alert')).toContainText('playback failed');
	expect(requests.storedOpens.map((request) => request.streamId)).toEqual(['main']);
	await expect(page.locator('[data-keep-player]')).toHaveAttribute(
		'data-recording-playhead-ms',
		String(momentMs)
	);
});

test('copy snapshots the video clock without a timeupdate and does not disturb playback', async ({
	page
}) => {
	const requests = await mockKeepModes(page, 1);
	await capturedClipboard(page);
	await page.goto(`/keep?camera=front-door&stream=auto&at=${momentMs}&token=private#session`);
	await expect(page.locator('[data-keep-player]')).toHaveAttribute(
		'data-recording-playhead-ms',
		String(momentMs)
	);
	await page.locator('video').evaluate((video) => {
		Object.defineProperty(video, 'readyState', { configurable: true, value: 2 });
		Object.defineProperty(video, 'currentTime', { configurable: true, value: 1.234 });
		video.dispatchEvent(new Event('loadeddata'));
	});
	const before = await playbackSnapshot(page);
	const sessionsBefore = requests.createAuthorizations.length;
	const opensBefore = requests.storedOpens.length;
	const seeksBefore = requests.storedSeeks.length;
	const subscriptionsBefore = requests.mediaSubscriptions.length;
	await page.getByRole('button', { name: copyCommand }).click();
	await expect(page.getByRole('status')).toHaveText('Recording link copied. Sign-in required.');
	const url = new URL(await copiedLink(page));
	expect(Number(url.searchParams.get('at'))).toBe(momentMs + 1234);
	expect(url.searchParams.get('stream')).toBe('auto');
	expect(url.searchParams.get('camera')).toBe('front-door');
	expect(url.href).not.toContain('private');
	expect(url.hash).toBe('');
	expect(await playbackSnapshot(page)).toEqual(before);
	expect(requests.createAuthorizations).toHaveLength(sessionsBefore);
	expect(requests.storedOpens).toHaveLength(opensBefore);
	expect(requests.storedSeeks).toHaveLength(seeksBefore);
	expect(requests.mediaSubscriptions).toHaveLength(subscriptionsBefore);
});

test('a fresh remote context must sign in before restoring a copied moment in another timezone', async ({
	page,
	browser
}) => {
	await mockKeepModes(page, 1);
	await capturedClipboard(page);
	await page.goto(`/keep?camera=front-door&stream=high&at=${momentMs}`);
	await page.getByRole('button', { name: copyCommand }).click();
	const link = await copiedLink(page);
	const accessKey = '550e8400-e29b-41d4-a716-446655440000';
	const fresh = await browser.newContext({ timezoneId: 'Pacific/Auckland' });
	try {
		const receiver = await fresh.newPage();
		const requests = await mockKeepModes(receiver, 1, {
			requiredAccessKey: accessKey,
			accessRole: 'user',
			accessLocal: false
		});
		await receiver.goto(link);
		await expect(receiver.getByRole('heading', { name: 'Remote sign-in' })).toBeVisible();
		expect(requests.storedOpens).toHaveLength(0);
		await receiver.getByLabel('Access key').fill(accessKey);
		await receiver.getByRole('button', { name: 'Sign in', exact: true }).click();
		await expect(receiver.locator('[data-keep-player]')).toHaveAttribute(
			'data-recording-playhead-ms',
			String(momentMs)
		);
		await expect(receiver.getByLabel('Quality')).toHaveValue('high');
		expect(requests.storedOpens[0]?.timestampMs).toBe(momentMs);
		expect(requests.storedOpens[0]?.sourceId).toBe('front-door');
		expect(link).not.toContain(accessKey);
		expect(new URL(receiver.url()).searchParams.get('date')).toBe(keepModeDate);
	} finally {
		await fresh.close();
	}
});

test('event copy matches Open at this moment and returns to the selected filtered event', async ({
	page
}) => {
	await mockEvents(page);
	await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
	await page.goto(`/events?date=${eventDate}&type=person&zone=porch`);
	await page.locator('[data-event-card="front-door:person-high"]').click();
	const drawer = page.getByRole('complementary', { name: 'Event detail' });
	const openHref = await drawer
		.getByRole('link', { name: 'Open at this moment' })
		.getAttribute('href');
	await drawer.getByRole('button', { name: copyCommand }).click();
	await expect(drawer.getByRole('status').filter({ hasText: 'Recording link copied.' })).toHaveText(
		'Recording link copied. Sign-in required.'
	);
	const link = await page.evaluate(() => navigator.clipboard.readText());
	expect(new URL(link).pathname + new URL(link).search).toBe(openHref);
	expect(new URL(link).searchParams.get('event')).toBe('person-high');
	await drawer.getByRole('link', { name: 'Open at this moment' }).click();
	await page.getByRole('link', { name: 'Back to event' }).click();
	await expect(page.getByLabel('Event type filter')).toHaveValue('person');
	await expect(page.getByLabel('Zone', { exact: true })).toHaveValue('porch');
	await expect(page.getByRole('complementary', { name: 'Event detail' })).toBeVisible();
});

test('Escape closes only the manual-copy dialog and restores the event copy command', async ({
	page
}) => {
	await page.setViewportSize({ width: 390, height: 844 });
	await mockEvents(page);
	await page.addInitScript(() => {
		Object.defineProperty(navigator, 'clipboard', {
			configurable: true,
			value: {
				writeText: async () => {
					throw new DOMException('Denied', 'NotAllowedError');
				}
			}
		});
	});
	await page.goto(`/events?date=${eventDate}`);
	await page.locator('[data-event-card="front-door:person-high"]').click();
	const drawer = page.getByRole('complementary', { name: 'Event detail' });
	const command = drawer.getByRole('button', { name: copyCommand });
	await command.focus();
	await page.keyboard.press('Enter');
	await expect(page.getByRole('dialog', { name: 'Copy recording link' })).toBeVisible();
	await expect(page.getByLabel('Authenticated recording link')).toBeFocused();
	await page.keyboard.press('Escape');
	await expect(page.getByRole('dialog', { name: 'Copy recording link' })).toHaveCount(0);
	await expect(drawer).toBeVisible();
	await expect(command).toBeFocused();
});

test('copy has bounded latency and no added playback work against a no-copy baseline', async ({
	page
}, testInfo) => {
	const requests = await mockKeepModes(page, 1);
	await capturedClipboard(page);
	await page.goto(`/keep?camera=front-door&stream=auto&at=${momentMs}`);
	const button = page.getByRole('button', { name: copyCommand });
	await expect(button).toBeEnabled();
	await page.locator('video').dispatchEvent('loadeddata');
	const before = await playbackSnapshot(page);
	const counters = () => ({
		sessions: requests.createAuthorizations.length,
		opens: requests.storedOpens.length,
		closes: requests.storedCloses.length,
		seeks: requests.storedSeeks.length,
		subscriptions: requests.mediaSubscriptions.length
	});
	const countersBefore = counters();
	const measurements = await button.evaluate(async (element) => {
		const command = element as HTMLButtonElement;
		const measure = async (copy: boolean) => {
			const samples: number[] = [];
			for (let iteration = 0; iteration < 20; iteration += 1) {
				await new Promise(requestAnimationFrame);
				const startedAt = performance.now();
				if (copy) command.click();
				await new Promise(requestAnimationFrame);
				if (copy && command.getAttribute('aria-busy') !== 'false')
					throw new Error('Copy did not settle');
				samples.push(performance.now() - startedAt);
			}
			const sorted = samples.toSorted((left, right) => left - right);
			return { samples, p50Ms: sorted[9], p95Ms: sorted[18] };
		};
		return { noCopy: await measure(false), copy: await measure(true) };
	});
	const evidence = { ...measurements, iterations: 20, before: countersBefore, after: counters() };
	const evidencePath = testInfo.outputPath('recording-link-performance.json');
	await writeFile(evidencePath, JSON.stringify(evidence, null, 2));
	await testInfo.attach('recording-link-performance.json', {
		path: evidencePath,
		contentType: 'application/json'
	});
	expect(measurements.copy.p95Ms).toBeLessThan(250);
	expect(counters()).toEqual(countersBefore);
	expect(await playbackSnapshot(page)).toEqual(before);
	expect(await page.evaluate(() => (window as RecordingLinkWindow).recordingLinks.length)).toBe(20);
});

test('copy controls and manual fallback fit desktop, tablet, and phone viewports', async ({
	page
}, testInfo) => {
	await mockKeepModes(page, 1);
	await page.addInitScript(() => {
		Object.defineProperty(navigator, 'clipboard', { configurable: true, value: undefined });
	});
	await page.goto(
		`/keep?camera=front-door&stream=auto&at=${momentMs}&returnTo=${encodeURIComponent(`/events?date=${keepModeDate}&event=front-door-person&eventCamera=front-door`)}`
	);
	const command = page.getByRole('button', { name: copyCommand });
	for (const viewport of [
		{ width: 1440, height: 900 },
		{ width: 768, height: 1024 },
		{ width: 390, height: 844 }
	]) {
		await page.setViewportSize(viewport);
		await expect(command).toBeInViewport({ ratio: 1 });
		const modes = page.locator('[data-keep-mode-switcher] button');
		for (const modeButton of await modes.all()) {
			await expect(modeButton).toBeInViewport({ ratio: 1 });
			expect(
				await modeButton.evaluate((element) => {
					const bounds = element.getBoundingClientRect();
					return [bounds.left + 2, bounds.left + bounds.width / 2, bounds.right - 2].every(
						(horizontal) =>
							document
								.elementFromPoint(horizontal, bounds.y + bounds.height / 2)
								?.closest('button') === element
					);
				})
			).toBe(true);
		}
		await attachScreenshot(page, testInfo, `recording-link-${viewport.width}.png`);
		await command.click();
		const dialog = page.getByRole('dialog', { name: 'Copy recording link' });
		await expect(dialog).toBeVisible();
		const bounds = await dialog.boundingBox();
		expect(bounds).not.toBeNull();
		expect(bounds!.x).toBeGreaterThanOrEqual(8);
		expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(viewport.width - 8);
		await expect(page.getByLabel('Authenticated recording link')).toBeFocused();
		await attachScreenshot(page, testInfo, `recording-link-fallback-${viewport.width}.png`);
		await page.getByRole('button', { name: 'Close copy dialog' }).click();
		await expect(command).toBeFocused();
	}
});

async function waitForRecordedFrame(page: Page): Promise<void> {
	await expect
		.poll(
			() =>
				page.locator('video').evaluateAll((elements) => {
					const video = elements[0] as HTMLVideoElement | undefined;
					return Boolean(video && video.readyState >= 2 && video.videoWidth > 0);
				}),
			{ timeout: 15_000 }
		)
		.toBe(true);
}

test('real H.264 playback reopens the copied clock in a fresh session within one second', async ({
	page,
	browser
}, testInfo) => {
	test.setTimeout(45_000);
	await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
	await page.goto('/keep?stream=main');
	await waitForRecordedFrame(page);
	const video = page.locator('video');
	await video.evaluate((element) => {
		(element as HTMLVideoElement).playbackRate = 0.25;
	});
	const source = await video.getAttribute('src');
	await page.getByRole('button', { name: copyCommand }).click();
	const link = await page.evaluate(() => navigator.clipboard.readText());
	const expectedTimestampMs = Number(new URL(link).searchParams.get('at'));
	expect(expectedTimestampMs).toBeGreaterThan(0);
	await expect(video).toHaveAttribute('src', source!);
	await expect(video).toHaveJSProperty('paused', false);
	const fresh = await browser.newContext({
		permissions: ['clipboard-read', 'clipboard-write'],
		timezoneId: 'America/Los_Angeles'
	});
	try {
		const receiver = await fresh.newPage();
		await receiver.goto(link);
		await waitForRecordedFrame(receiver);
		const colors = await receiver.locator('video').evaluate((element) => {
			const video = element as HTMLVideoElement;
			video.pause();
			const canvas = document.createElement('canvas');
			canvas.width = 64;
			canvas.height = 36;
			const context = canvas.getContext('2d')!;
			context.drawImage(video, 0, 0, 64, 36);
			const pixels = context.getImageData(0, 0, 64, 36).data;
			const distinct = new Set<number>();
			for (let offset = 0; offset < pixels.length; offset += 4)
				distinct.add((pixels[offset] << 16) | (pixels[offset + 1] << 8) | pixels[offset + 2]);
			return distinct.size;
		});
		expect(colors).toBeGreaterThan(16);
		await receiver.getByRole('button', { name: copyCommand }).click();
		const restored = new URL(await receiver.evaluate(() => navigator.clipboard.readText()));
		const errorMs = Math.abs(Number(restored.searchParams.get('at')) - expectedTimestampMs);
		expect(restored.searchParams.get('camera')).toBe(new URL(link).searchParams.get('camera'));
		expect(errorMs).toBeLessThanOrEqual(1000);
		await testInfo.attach('recording-link-real-playback.json', {
			body: JSON.stringify({
				expectedTimestampMs,
				restoredTimestampMs: Number(restored.searchParams.get('at')),
				errorMs,
				colors
			}),
			contentType: 'application/json'
		});
		await attachScreenshot(receiver, testInfo, 'recording-link-real-playback.png');
	} finally {
		await fresh.close();
	}
});
