import { expect, test, type Locator, type Page } from '@playwright/test';
import { resolve } from 'node:path';
import {
	cardAdminKey,
	cardSourceIds,
	reviewSourceIds,
	startHomeAssistantServer
} from './fixtures/home-assistant-server';
import {
	installCardProbe,
	interruptCardPeer,
	readCardProbe,
	readRecordedCursorBudget
} from './fixtures/home-assistant-browser';
import { recordedAudioBackendRoute } from './fixtures/recorded-audio-playback';
import type { RecordingCoverageResponse } from '../src/lib/types';

test.use({ timezoneId: 'America/New_York' });

async function displayed(video: Locator) {
	return video.evaluate((element: HTMLVideoElement) => ({
		seconds: element.currentTime,
		paused: element.paused,
		muted: element.muted,
		decoded: element.getVideoPlaybackQuality().totalVideoFrames,
		ready: element.readyState
	}));
}

async function presentedDrift(page: Page, count = 2) {
	return page.locator('article time').evaluateAll((times, expected) => {
		if (times.length !== expected) return null;
		const UTC = times.map((t) => Date.parse(t.getAttribute('datetime') ?? ''));
		return UTC.every(Number.isFinite) ? Math.max(...UTC) - Math.min(...UTC) : null;
	}, count);
}

async function measurePresentedDrift(page: Page, count = 2) {
	const drift: number[] = [];
	const until = performance.now() + 1500;
	while (performance.now() < until) {
		await page.evaluate(
			() => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()))
		);
		const sample = await presentedDrift(page, count);
		if (sample !== null) drift.push(sample);
	}
	expect(drift.length).toBeGreaterThan(4);
	drift.sort((a, b) => a - b);
	await test.info().attach('presented-camera-drift-ms', {
		contentType: 'application/json',
		body: JSON.stringify({
			count: drift.length,
			median: drift[Math.floor(drift.length / 2)],
			p95: drift[Math.floor(drift.length * 0.95)],
			maximum: drift.at(-1)
		})
	});
	expect(drift[Math.floor(drift.length * 0.95)]).toBeLessThanOrEqual(250);
}

async function overlappingMoment(page: Page, backend: string, crossMinute = false) {
	const response = await page.request.get(backend + '/recording-coverage?stream=sub', {
		headers: { Authorization: 'Bearer ' + cardAdminKey }
	});
	expect(response.ok()).toBeTruthy();
	const coverage: RecordingCoverageResponse = await response.json();
	const ranges = cardSourceIds.map(
		(id) =>
			coverage.cameras
				.find((camera) => camera.camera_id === id)
				?.streams.find((stream) => stream.stream_id === 'sub')?.ranges[0]
	);
	if (!ranges[0] || !ranges[1]) throw new Error('The recorder must index both seeded streams.');
	const start = Math.max(ranges[0].start_ms, ranges[1].start_ms);
	const at = crossMinute ? Math.ceil(start / 60_000) * 60_000 - 900 : start + 600;
	expect(at).toBeGreaterThan(start);
	expect(at + 6000).toBeLessThan(Math.min(ranges[0].end_ms, ranges[1].end_ms));
	return at;
}

async function checkLimitedDecoder(page: Page) {
	await installCardProbe(page);
	await page.addInitScript(() =>
		Object.defineProperty(Navigator.prototype, 'hardwareConcurrency', {
			configurable: true,
			get: () => 2
		})
	);
	await page.reload();
	await page.setViewportSize({ width: 390, height: 844 });
	await expect(page.locator('article video')).toHaveCount(1);
	await expect(page.getByText('Waiting for a decoder')).toBeVisible();
	await page.getByRole('button', { name: 'Comparison options' }).click();
	await page.getByRole('combobox', { name: 'Focused camera' }).selectOption(cardSourceIds[1]);
	await expect(page.getByRole('article', { name: 'card-camera-1' }).locator('video')).toHaveCount(
		1
	);
	await expect(
		page.getByRole('article', { name: 'card-camera-0' }).getByText('Waiting for a decoder')
	).toBeVisible();
	const budget = () => readRecordedCursorBudget(page);
	await expect.poll(async () => (await budget()).active).toEqual([cardSourceIds[1]]);
	await page.evaluate(() => {
		Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' });
		document.dispatchEvent(new Event('visibilitychange'));
	});
	await expect(page.locator('article video')).toHaveCount(0);
	await expect.poll(async () => (await budget()).active).toEqual([]);
	await page.evaluate(() => {
		Reflect.deleteProperty(document, 'visibilityState');
		document.dispatchEvent(new Event('visibilitychange'));
	});
	await expect(page.getByRole('article', { name: 'card-camera-1' }).locator('video')).toHaveCount(
		1
	);
	await expect.poll(budget).toEqual({
		peers: 1,
		active: [cardSourceIds[1]],
		peakCursors: 1,
		maximumRequestedBufferMs: 10_000
	});
	await test.info().attach('compact-cursor-budget', {
		body: JSON.stringify(await budget()),
		contentType: 'application/json'
	});
}

async function checkSharedPlayAndPause(page: Page, tiles: Locator) {
	await page.getByRole('button', { name: 'Play recordings', pressed: false }).click();
	await expect(page.getByRole('button', { name: 'Play recordings', pressed: true })).toBeVisible();
	const starts = await Promise.all([displayed(tiles.nth(0)), displayed(tiles.nth(1))]);
	for (const index of [0, 1])
		await expect
			.poll(async () => (await displayed(tiles.nth(index))).seconds)
			.toBeGreaterThan(starts[index]!.seconds + 0.35);
	await expect.poll(async () => (await presentedDrift(page)) ?? Infinity).toBeLessThanOrEqual(250);
	await measurePresentedDrift(page);
	await page.getByRole('button', { name: 'Play recordings', pressed: true }).click();
	for (const index of [0, 1])
		await expect.poll(async () => (await displayed(tiles.nth(index))).paused).toBe(true);
}

test('two recorded cameras follow one clock, seek through coverage gaps and switch audio', async ({
	page,
	context,
	baseURL
}) => {
	test.setTimeout(90_000);
	page.setDefaultTimeout(8_000);
	if (!baseURL) throw new Error('The two-camera fixture requires a UI origin.');
	await page.addInitScript(() => {
		const key = 'keeppeek-playback-preferences';
		if (!localStorage.getItem(key))
			localStorage.setItem(key, JSON.stringify({ version: 1, media: { playbackRate: 100 } }));
	});
	const origin = new URL(baseURL).origin;
	const server = await startHomeAssistantServer(origin, {
		trustedLocal: true,
		reviewFixture: true
	});
	const redirect = recordedAudioBackendRoute(origin, server.url);
	try {
		await context.route('**/*', redirect);
		const at = await overlappingMoment(page, server.url);
		await page.goto('/keep?camera=' + cardSourceIds[0] + '&stream=sub&at=' + at);
		await expect(page.getByLabel('Compare with')).toBeVisible({ timeout: 30_000 });
		await page.getByLabel('Compare with').selectOption(cardSourceIds[1]);
		await expect(page.getByRole('heading', { name: 'Compare recordings' })).toBeVisible();
		await expect(page.getByRole('combobox', { name: 'Playback speed' })).toHaveValue('1');
		const tiles = page.locator('article video');
		await expect(tiles).toHaveCount(2);
		for (const index of [0, 1])
			await expect.poll(async () => (await displayed(tiles.nth(index))).decoded).toBeGreaterThan(0);
		await checkSharedPlayAndPause(page, tiles);
		const unmute = page.getByRole('button', { name: 'Mute audio', pressed: true });
		if (await unmute.isVisible()) await unmute.click();
		await page
			.getByRole('combobox', { name: 'Audio source', exact: true })
			.selectOption(cardSourceIds[1]);
		await expect
			.poll(async () =>
				(await Promise.all([displayed(tiles.nth(0)), displayed(tiles.nth(1))])).map((v) => v.muted)
			)
			.toEqual([true, false]);
		await expect(page).toHaveURL(/audio=192\.0\.2\.102/);
		await page.getByRole('button', { name: 'Back five seconds' }).click();
		await expect(page.getByText('No playable footage at this time')).toBeVisible();
		await expect(tiles).toHaveCount(1);
		await page.getByRole('button', { name: 'Forward five seconds' }).click();
		await expect(tiles).toHaveCount(2);
		for (const index of [0, 1])
			await expect.poll(async () => (await displayed(tiles.nth(index))).decoded).toBeGreaterThan(0);
		await checkLimitedDecoder(page);
		await page.getByRole('button', { name: 'Single camera' }).click();
		await expect(page.getByRole('heading', { name: 'Keep', exact: true })).toBeVisible();
	} finally {
		try {
			await context.unroute('**/*', redirect);
		} finally {
			await server.close();
		}
	}
});

test('only the chosen calibrated AAC camera is unmuted on reload and after an audio switch', async ({
	page,
	context,
	baseURL
}) => {
	test.setTimeout(90_000);
	page.setDefaultTimeout(8_000);
	if (!baseURL) throw new Error('The comparison fixture requires a UI origin.');
	const origin = new URL(baseURL).origin;
	const fixture = await startHomeAssistantServer(origin, {
		trustedLocal: true,
		calibrationFixture: {
			mediaPath: resolve(
				import.meta.dirname,
				'../../crates/test-camera/testdata/flash-tone-lead-200ms.mp4'
			),
			bothCameras: true
		}
	});
	const redirect = recordedAudioBackendRoute(origin, fixture.url);
	try {
		await context.route('**/*', redirect);
		const manifests = [fixture.calibration, fixture.secondCalibration];
		const from = Math.max(...manifests.map((m) => m?.start_ms ?? Infinity)) + 120;
		const until = Math.min(...manifests.map((m) => m?.end_ms ?? 0));
		expect(until - from).toBeGreaterThan(1200);
		const query = new URLSearchParams({
			camera: cardSourceIds[0],
			stream: 'sub',
			at: String(from),
			audio: cardSourceIds[1]
		});
		for (const id of cardSourceIds) query.append('compare', id);
		await page.goto('/keep?' + query);
		await expect(page.locator('article video')).toHaveCount(2);
		await page.reload();
		await expect(page.getByRole('combobox', { name: 'Audio source' })).toHaveValue(
			cardSourceIds[1]
		);
		const unmute = page.getByRole('button', { name: 'Mute audio', pressed: true });
		if (await unmute.isVisible()) await unmute.click();
		await page.getByRole('button', { name: 'Play recordings', pressed: false }).click();
		const witness = () =>
			page.locator('article video').evaluateAll((elements) =>
				elements.map((el) => {
					const video = el as HTMLVideoElement & { webkitAudioDecodedByteCount?: number };
					return { muted: video.muted, decoded: (video.webkitAudioDecodedByteCount ?? 0) > 0 };
				})
			);
		await expect.poll(witness).toEqual([
			{ muted: true, decoded: true },
			{ muted: false, decoded: true }
		]);
		await page.getByRole('combobox', { name: 'Audio source' }).selectOption(cardSourceIds[0]);
		await expect.poll(witness).toEqual([
			{ muted: false, decoded: true },
			{ muted: true, decoded: true }
		]);
	} finally {
		try {
			await context.unroute('**/*', redirect);
		} finally {
			await fixture.close();
		}
	}
});

async function verifyComparisonReload(page: Page, origin: string, seek: number) {
	await page.getByRole('combobox', { name: 'Playback speed' }).selectOption('2');
	await page.context().grantPermissions(['clipboard-read', 'clipboard-write'], { origin });
	await page.getByRole('button', { name: 'Copy link to this moment (sign-in required)' }).click();
	const copied = new URL(await page.evaluate(() => navigator.clipboard.readText()));
	expect(copied.searchParams.get('at')).toBe(String(seek));
	expect(copied.searchParams.getAll('compare')).toEqual([...reviewSourceIds]);
	expect(copied.searchParams.get('audio')).toBe(reviewSourceIds[2]);
	await page.reload();
	await expect(page.getByRole('combobox', { name: 'Playback speed' })).toHaveValue('2');
	await expect(page.getByRole('combobox', { name: 'Focused camera' })).toHaveValue(
		reviewSourceIds[3]
	);
	await expect(page.getByRole('combobox', { name: 'Audio source' })).toHaveValue(
		reviewSourceIds[2]
	);
	expect(new URL(page.url()).searchParams.getAll('compare')).toEqual([...reviewSourceIds]);
}

async function verifyContinuousMinute(page: Page, at: number) {
	await page.getByLabel('Seek to UTC').fill(new Date(at).toISOString().slice(0, 23));
	await page.getByRole('button', { name: 'Go to time' }).click();
	const videos = page.locator('article video');
	const times = () =>
		page
			.locator('article time')
			.evaluateAll((elements) =>
				elements.map((element) => Date.parse(element.getAttribute('datetime') ?? ''))
			);
	const sources = () =>
		videos.evaluateAll((elements) =>
			elements.map((element) => (element as HTMLVideoElement).currentSrc)
		);
	await expect
		.poll(async () => (await times()).filter((time) => Math.abs(time - at) <= 150).length)
		.toBe(4);
	const beforeBoundary = await sources();
	await page.getByRole('button', { name: 'Play recordings', pressed: false }).click();
	await expect
		.poll(() =>
			videos.evaluateAll(
				(elements) =>
					elements.filter((element) => {
						const video = element as HTMLVideoElement;
						return !video.paused && video.getVideoPlaybackQuality().totalVideoFrames > 2;
					}).length
			)
		)
		.toBe(4);
	await measurePresentedDrift(page, 4);
	const boundary = Math.ceil(at / 60_000) * 60_000;
	await expect.poll(async () => (await times()).filter((time) => time >= boundary).length).toBe(4);
	expect(await sources()).toEqual(beforeBoundary);
}

async function verifyContinuingRecording(page: Page, boundary: number) {
	// Three recordings end near +25 s. Camera 1 extends past the first cursor's finite +30 s bound.
	await page.getByRole('combobox', { name: 'Playback speed' }).selectOption('4');
	const continuing = page.getByRole('article', { name: 'card-camera-1' });
	await expect
		.poll(
			() =>
				continuing
					.locator('time')
					.evaluateAll((times) => Date.parse(times[0]?.getAttribute('datetime') ?? '')),
			{ timeout: 15_000, intervals: [100] }
		)
		.toBeGreaterThan(boundary + 30_250);
	await page.getByRole('button', { name: 'Play recordings', pressed: true }).click();
	await expect(page.getByText('No playable footage at this time')).toHaveCount(3);
	await expect(page.locator('article video')).toHaveCount(1);
	await expect
		.poll(async () => (await readRecordedCursorBudget(page)).active)
		.toEqual([cardSourceIds[1]]);
}

test('add and remove four authorized sources and seek their common UTC without transient links', async ({
	page,
	context,
	baseURL
}) => {
	test.setTimeout(90_000);
	page.setDefaultTimeout(8_000);
	if (!baseURL) throw new Error('The comparison fixture requires a UI origin.');
	await installCardProbe(page);
	await page.setViewportSize({ width: 1440, height: 1100 });
	await page.addInitScript(() =>
		Object.defineProperty(Navigator.prototype, 'hardwareConcurrency', {
			configurable: true,
			get: () => 8
		})
	);
	const origin = new URL(baseURL).origin;
	const fixture = await startHomeAssistantServer(origin, {
		trustedLocal: true,
		reviewFixture: 'four'
	});
	const redirect = recordedAudioBackendRoute(origin, fixture.url);
	try {
		await context.route('**/*', redirect);
		const at = await overlappingMoment(page, fixture.url, true);
		await page.goto('/keep?camera=' + cardSourceIds[0] + '&stream=sub&at=' + at);
		await expect(page.getByLabel('Compare with')).toBeVisible({ timeout: 30_000 });
		await page.getByLabel('Compare with').selectOption(cardSourceIds[1]);
		const add = page.getByRole('combobox', { name: 'Add camera' });
		for (const id of reviewSourceIds.slice(2)) await add.selectOption(id);
		await expect(add).toHaveCount(0);
		await expect(page.locator('article')).toHaveCount(4);
		await expect(page.locator('article video')).toHaveCount(4);
		await expect
			.poll(async () => (await readRecordedCursorBudget(page)).active.toSorted())
			.toEqual([...reviewSourceIds].toSorted());
		const resources = await readRecordedCursorBudget(page);
		expect(resources.peers).toBe(1);
		expect(resources.peakCursors).toBe(4);
		expect(resources.maximumRequestedBufferMs).toBe(10_000);
		await test.info().attach('four-camera-cursor-budget', {
			body: JSON.stringify(resources),
			contentType: 'application/json'
		});
		await verifyContinuousMinute(page, at);
		await verifyContinuingRecording(page, Math.ceil(at / 60_000) * 60_000);
		const seek = at + 5000;
		await page.getByLabel('Seek to UTC').fill(new Date(seek).toISOString().slice(0, 23));
		await page.getByRole('button', { name: 'Go to time' }).click();
		await expect.poll(() => new URL(page.url()).searchParams.get('at')).toBe(String(seek));
		await page.getByRole('combobox', { name: 'Focused camera' }).selectOption(reviewSourceIds[3]);
		await page.getByRole('combobox', { name: 'Audio source' }).selectOption(reviewSourceIds[2]);
		await verifyComparisonReload(page, origin, seek);
		await page.getByRole('button', { name: 'Remove card-camera-3' }).click();
		await expect(page.getByRole('combobox', { name: 'Focused camera' })).toHaveValue(
			cardSourceIds[0]
		);
		await page.getByRole('button', { name: 'Remove card-camera-2' }).click();
		await expect(page.getByRole('combobox', { name: 'Audio source' })).toHaveValue(
			cardSourceIds[0]
		);
		await page.getByRole('button', { name: 'Remove card-camera-1' }).click();
		await expect(page.getByRole('heading', { name: 'Keep', exact: true })).toBeVisible();
		expect(new URL(page.url()).searchParams.has('compare')).toBe(false);
	} finally {
		try {
			await context.unroute('**/*', redirect);
		} finally {
			await fixture.close();
		}
	}
});

async function installDecoderFailure(page: Page) {
	await page.addInitScript(() => {
		const native = URL.createObjectURL.bind(URL);
		const sources = new Map<string, MediaSource>();
		URL.createObjectURL = (object) => {
			const url = native(object);
			if (object instanceof MediaSource) sources.set(url, object);
			return url;
		};
		Object.assign(window, {
			failSelectedDecoder: (url: string) => {
				const source = sources.get(url);
				if (
					source?.readyState !== 'open' ||
					[...source.sourceBuffers].some((buffer) => buffer.updating)
				)
					return false;
				source.endOfStream('decode');
				return true;
			}
		});
	});
}

test('a browser decoder failure releases only its tile and retry rejoins the advancing clock', async ({
	page,
	context,
	baseURL
}) => {
	test.setTimeout(75_000);
	page.setDefaultTimeout(8_000);
	if (!baseURL) throw new Error('The decoder fixture requires a UI origin.');
	await installDecoderFailure(page);
	const origin = new URL(baseURL).origin;
	const server = await startHomeAssistantServer(origin, {
		trustedLocal: true,
		reviewFixture: true
	});
	const redirect = recordedAudioBackendRoute(origin, server.url);
	try {
		await context.route('**/*', redirect);
		const at = await overlappingMoment(page, server.url);
		const query = new URLSearchParams({ camera: cardSourceIds[0], at: String(at), stream: 'sub' });
		for (const id of cardSourceIds) query.append('compare', id);
		await page.goto('/keep?' + query);
		const healthy = page.getByRole('article', { name: 'card-camera-0' }).locator('video');
		const affected = page.getByRole('article', { name: 'card-camera-1' });
		const video = affected.locator('video');
		await expect(page.getByRole('button', { name: 'Play recordings', pressed: false })).toBeVisible(
			{ timeout: 30_000 }
		);
		await page.getByRole('button', { name: 'Play recordings', pressed: false }).click();
		await expect.poll(async () => (await displayed(video)).decoded).toBeGreaterThan(3);
		const before = (await displayed(healthy)).seconds;
		await expect
			.poll(() =>
				video.evaluate((v: HTMLVideoElement) =>
					(
						window as Window & { failSelectedDecoder?: (url: string) => boolean }
					).failSelectedDecoder?.(v.currentSrc)
				)
			)
			.toBe(true);
		await expect(affected.getByRole('button', { name: 'Retry recording' })).toBeVisible();
		await expect(video).toHaveCount(0);
		await expect.poll(async () => (await displayed(healthy)).seconds).toBeGreaterThan(before + 0.5);
		await affected.getByRole('button', { name: 'Retry recording' }).click();
		await expect.poll(async () => (await displayed(video)).decoded).toBeGreaterThan(1);
		await expect
			.poll(async () => (await presentedDrift(page)) ?? Infinity)
			.toBeLessThanOrEqual(250);
	} finally {
		try {
			await context.unroute('**/*', redirect);
		} finally {
			await server.close();
		}
	}
});

test('an interrupted comparison clears old frames and returns paused at its last shared moment', async ({
	page,
	context,
	baseURL
}) => {
	test.setTimeout(75_000);
	if (!baseURL) throw new Error('The interrupted comparison requires a UI origin.');
	await installCardProbe(page);
	const origin = new URL(baseURL).origin;
	const server = await startHomeAssistantServer(origin, {
		trustedLocal: true,
		reviewFixture: true
	});
	const redirect = recordedAudioBackendRoute(origin, server.url);
	try {
		await context.route('**/*', redirect);
		const at = await overlappingMoment(page, server.url);
		const query = new URLSearchParams({
			camera: cardSourceIds[0],
			at: String(at),
			stream: 'sub',
			audio: cardSourceIds[1]
		});
		for (const id of cardSourceIds) query.append('compare', id);
		await page.goto('/keep?' + query);
		const play = page.getByRole('button', { name: 'Play recordings' });
		const videos = page.locator('article video');
		const firstTime = () =>
			page
				.locator('article time')
				.first()
				.evaluate((node) => Date.parse(node.getAttribute('datetime') ?? ''));
		await expect(videos).toHaveCount(2, { timeout: 30_000 });
		await play.click();
		await expect.poll(firstTime).toBeGreaterThan(at + 1_400);
		const seen = await firstTime();
		await interruptCardPeer(page);
		await expect(videos).toHaveCount(0);
		await page.getByRole('button', { name: 'Retry sign-in', exact: true }).click({ timeout: 8000 });
		await expect(videos).toHaveCount(2, { timeout: 15_000 });
		await expect(play).toHaveAttribute('aria-pressed', 'false');
		await expect(page.getByRole('combobox', { name: 'Audio source', exact: true })).toHaveValue(
			cardSourceIds[1]
		);
		await expect.poll(firstTime).toBeGreaterThanOrEqual(seen - 250);
		const peers = await readCardProbe(page);
		expect(peers.created - peers.closed).toBe(1);
		expect(peers.overflow).toBe(false);
	} finally {
		try {
			await context.unroute('**/*', redirect);
		} finally {
			await server.close();
		}
	}
});

test('a comparison with an unavailable focused camera keeps its authorized recordings playable', async ({
	page,
	context,
	baseURL
}) => {
	test.setTimeout(45_000);
	if (!baseURL) throw new Error('The partial comparison requires a UI origin.');
	await installCardProbe(page);
	const origin = new URL(baseURL).origin;
	const server = await startHomeAssistantServer(origin, {
		trustedLocal: true,
		reviewFixture: true
	});
	const redirect = recordedAudioBackendRoute(origin, server.url);
	try {
		await context.route('**/*', redirect);
		const at = await overlappingMoment(page, server.url);
		const missing = 'removed-recorded-camera';
		const query = new URLSearchParams({
			camera: missing,
			at: String(at),
			stream: 'sub',
			audio: cardSourceIds[1]
		});
		for (const id of [missing, ...cardSourceIds]) query.append('compare', id);
		await page.goto('/keep?' + query);
		const videos = page.locator('article video');
		await expect(videos).toHaveCount(2, { timeout: 10000 });
		const unavailable = page.getByRole('article', { name: 'Unavailable camera' });
		await expect(unavailable).toBeVisible();
		await expect(unavailable.locator('video')).toHaveCount(0);
		await expect(page.getByRole('combobox', { name: 'Focused camera' })).toHaveValue(
			cardSourceIds[0]
		);
		await expect(page.getByRole('combobox', { name: 'Audio source', exact: true })).toHaveValue(
			cardSourceIds[1]
		);
		await checkSharedPlayAndPause(page, videos);
		const peers = await readCardProbe(page);
		expect(peers.created - peers.closed).toBe(1);
	} finally {
		try {
			await context.unroute('**/*', redirect);
		} finally {
			await server.close();
		}
	}
});

type PausedFrameObservation = {
	latePicture: boolean;
	marked: boolean;
	recovered: boolean;
	lateFrameMs: number;
	recoveredFrameErrorMs: number;
	elapsedMs: number;
	seekRequests: number;
};
type ObservedReviewTile = HTMLElement & {
	alignmentWitness?: PausedFrameObservation;
	disposeAlignmentWitness?: () => void;
};

type PausedDecoderCase = 'native' | 'ahead-clock' | 'stalled';

async function verifyPausedFrameCorrection(
	tile: Locator,
	target: number,
	scenario: PausedDecoderCase
) {
	await tile.evaluate(
		(element, options) => {
			const el = element as ObservedReviewTile;
			const video = el.querySelector('video')!;
			const native = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'currentTime');
			if (!native?.get || !native.set)
				throw new Error('The browser must expose its native media clock.');
			const before = video.currentTime;
			const observation: PausedFrameObservation = {
				latePicture: false,
				marked: false,
				recovered: false,
				elapsedMs: 0,
				lateFrameMs: 0,
				recoveredFrameErrorMs: 0,
				seekRequests: 0
			};
			el.alignmentWitness = observation;
			if (options.scenario !== 'native') {
				// Emulate an updated seek clock while Chromium still presents its earlier, real MP4 frame.
				Object.defineProperty(video, 'currentTime', {
					configurable: true,
					get: () => before,
					set: (value: number) => {
						observation.seekRequests++;
						if (options.scenario !== 'stalled') native.set!.call(video, value);
					}
				});
			}
			let began: number | undefined;
			const observer = new MutationObserver(() => {
				const time = Date.parse(el.querySelector('time')?.dateTime ?? '');
				const correction = el.querySelector('header [role=status]');
				if (time < options.target - 450) {
					observation.latePicture = true;
					if (began === undefined) {
						began = performance.now();
						observation.lateFrameMs = options.target - time;
					}
					if (correction?.textContent?.includes('Synchronizing')) observation.marked = true;
				}
				if (observation.marked && Math.abs(time - options.target) <= 250 && !correction) {
					observation.recovered = true;
					observation.elapsedMs = Math.round(performance.now() - began!);
					observation.recoveredFrameErrorMs = Math.abs(time - options.target);
					observer.disconnect();
				}
			});
			observer.observe(el, {
				subtree: true,
				childList: true,
				characterData: true,
				attributes: true
			});
			el.disposeAlignmentWitness = () => {
				observer.disconnect();
				if (options.scenario !== 'native') Reflect.deleteProperty(video, 'currentTime');
				delete el.alignmentWitness;
				delete el.disposeAlignmentWitness;
			};
			native.set.call(video, before - 0.75);
		},
		{ target, scenario }
	);
	try {
		const read = () => tile.evaluate((el) => (el as ObservedReviewTile).alignmentWitness);
		await expect.poll(async () => (await read())?.latePicture).toBe(true);
		if (scenario === 'stalled') {
			const retry = tile.getByRole('button', { name: 'Retry recording' });
			await expect(tile.getByRole('status')).toHaveText('Alignment paused. Retry recording.');
			await expect(retry).toBeVisible();
			await expect
				.poll(read)
				.toMatchObject({ latePicture: true, marked: true, recovered: false, seekRequests: 2 });
			await retry.click();
		}
		await expect.poll(read).toMatchObject({ latePicture: true, marked: true, recovered: true });
		const measured = await read();
		expect(measured?.elapsedMs).toBeLessThanOrEqual(scenario === 'stalled' ? 5_000 : 1_500);
		if (scenario !== 'native') {
			expect(measured?.seekRequests).toBeGreaterThan(0);
			expect(measured?.seekRequests).toBeLessThanOrEqual(2);
		}
		await test.info().attach('paused-frame-' + scenario, {
			contentType: 'application/json',
			body: JSON.stringify(measured)
		});
	} finally {
		await tile.evaluate((el) => (el as ObservedReviewTile).disposeAlignmentWitness?.());
	}
}

test('a late paused picture stays identifiable until a corrected camera frame is presented', async ({
	page,
	context,
	baseURL
}) => {
	test.setTimeout(75_000);
	if (!baseURL) throw new Error('The frame alignment fixture requires a UI origin.');
	const origin = new URL(baseURL).origin;
	const server = await startHomeAssistantServer(origin, {
		trustedLocal: true,
		reviewFixture: true
	});
	const redirect = recordedAudioBackendRoute(origin, server.url);
	try {
		await context.route('**/*', redirect);
		const at = await overlappingMoment(page, server.url);
		const query = new URLSearchParams({ camera: cardSourceIds[0], at: String(at), stream: 'sub' });
		for (const id of cardSourceIds) query.append('compare', id);
		await page.goto('/keep?' + query);
		const tile = page.getByRole('article', { name: 'card-camera-0' });
		const video = tile.locator('video');
		const play = page.getByRole('button', { name: 'Play recordings' });
		await play.click({ timeout: 30_000 });
		await expect
			.poll(() =>
				video.evaluate((v: HTMLVideoElement) =>
					v.buffered.length ? v.currentTime - v.buffered.start(0) : 0
				)
			)
			.toBeGreaterThan(1.6);
		await play.click();
		await expect.poll(async () => (await displayed(video)).paused).toBe(true);
		const target = Number(new URL(page.url()).searchParams.get('at'));
		await verifyPausedFrameCorrection(tile, target, 'native');
		await verifyPausedFrameCorrection(tile, target, 'ahead-clock');
		await verifyPausedFrameCorrection(tile, target, 'stalled');
		await expect(play).toHaveAttribute('aria-pressed', 'false');
	} finally {
		try {
			await context.unroute('**/*', redirect);
		} finally {
			await server.close();
		}
	}
});
