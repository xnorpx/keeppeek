import { expect, test, type Locator, type Page } from '@playwright/test';
import { resolve } from 'node:path';
import { readFile } from 'node:fs/promises';
import {
	cardAdminKey,
	cardSourceIds,
	reviewSourceIds,
	startHomeAssistantServer
} from './fixtures/home-assistant-server';
import { recordedAudioBackendRoute } from './fixtures/recorded-audio-playback';
import type { RecordingCoverageResponse } from '../src/lib/types';

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

async function overlappingMoment(page: Page, backend: string) {
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
	const at = Math.max(ranges[0].start_ms, ranges[1].start_ms) + 600;
	expect(at + 6000).toBeLessThan(Math.min(ranges[0].end_ms, ranges[1].end_ms));
	return at;
}

async function checkLimitedDecoder(page: Page) {
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
	await page.getByRole('combobox', { name: 'Focused camera' }).selectOption(cardSourceIds[1]);
	await expect(page.getByRole('article', { name: 'card-camera-1' }).locator('video')).toHaveCount(
		1
	);
	await expect(
		page.getByRole('article', { name: 'card-camera-0' }).getByText('Waiting for a decoder')
	).toBeVisible();
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

test('add and remove four authorized sources and seek their common UTC without transient links', async ({
	page,
	context,
	baseURL
}) => {
	test.setTimeout(90_000);
	page.setDefaultTimeout(8_000);
	if (!baseURL) throw new Error('The comparison fixture requires a UI origin.');
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
		const at = await overlappingMoment(page, fixture.url);
		await page.goto('/keep?camera=' + cardSourceIds[0] + '&stream=sub&at=' + at);
		await expect(page.getByLabel('Compare with')).toBeVisible({ timeout: 30_000 });
		await page.getByLabel('Compare with').selectOption(cardSourceIds[1]);
		const add = page.getByRole('combobox', { name: 'Add camera' });
		for (const id of reviewSourceIds.slice(2)) await add.selectOption(id);
		await expect(add).toHaveCount(0);
		await expect(page.locator('article')).toHaveCount(4);
		await expect(page.locator('article video')).toHaveCount(4);
		await page.getByRole('button', { name: 'Play recordings', pressed: false }).click();
		await expect
			.poll(() =>
				page.locator('article video').evaluateAll(
					(elements) =>
						elements.filter((el) => {
							const video = el as HTMLVideoElement;
							return !video.paused && video.getVideoPlaybackQuality().totalVideoFrames > 2;
						}).length
				)
			)
			.toBe(4);
		await measurePresentedDrift(page, 4);
		await page.getByRole('button', { name: 'Play recordings', pressed: true }).click();
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

test('exports one focused source and retains the shared cameras, playhead and audio on return', async ({
	page,
	context,
	baseURL
}) => {
	test.setTimeout(90_000);
	if (!baseURL) throw new Error('The export fixture requires a UI origin.');
	const origin = new URL(baseURL).origin;
	const server = await startHomeAssistantServer(origin, {
		trustedLocal: true,
		reviewFixture: true
	});
	const redirect = recordedAudioBackendRoute(origin, server.url);
	try {
		await context.route('**/*', redirect);
		const response = await page.request.get(server.url + '/recording-coverage?stream=sub', {
			headers: { Authorization: 'Bearer ' + cardAdminKey }
		});
		expect(response.ok()).toBeTruthy();
		const coverage: RecordingCoverageResponse = await response.json();
		const end = coverage.cameras
			.find((row) => row.camera_id === cardSourceIds[1])
			?.streams.find((stream) => stream.stream_id === 'sub')
			?.ranges.at(-1)?.end_ms;
		if (end === undefined)
			throw new Error('The fixture must index the focused recording boundary.');
		const at = end - 5000;
		const [first, second] = cardSourceIds;
		const initial = new URLSearchParams({ camera: first, stream: 'sub', at: String(at) });
		initial.append('compare', first);
		initial.append('compare', second);
		await page.goto('/keep?' + initial);
		await page.getByRole('combobox', { name: 'Focused camera' }).selectOption(second);
		await expect(page.getByRole('article', { name: 'card-camera-1' }).locator('video')).toHaveCount(
			1
		);
		await page.getByRole('button', { name: 'Set range start' }).click();
		await page.getByRole('button', { name: 'Forward five seconds' }).click();
		await page.getByRole('button', { name: 'Set range end' }).click();
		await expect(
			page
				.getByRole('article', { name: 'card-camera-1' })
				.getByText('No playable footage at this time')
		).toBeVisible();
		await page.getByRole('button', { name: 'Forward five seconds' }).click();
		await page.getByRole('button', { name: 'Export focused camera' }).click();
		const panel = page.locator('[data-keep-export]');
		await expect(panel).toHaveAttribute('data-export-start-ms', String(at));
		await expect(panel).toHaveAttribute('data-export-end-ms', String(at + 5000));
		expect(new URL(page.url()).searchParams.get('camera')).toBe(second);
		expect(new URL(page.url()).searchParams.get('stream')).toBe('sub');
		const exportUrl = page.url();
		await page.reload();
		await expect(panel).toHaveAttribute('data-export-start-ms', String(at));
		await panel.getByRole('button', { name: 'Create export', exact: true }).click();
		await expect(panel).toHaveAttribute('data-export-status', 'ready', { timeout: 30_000 });
		const downloaded = page.waitForEvent('download');
		await panel.getByRole('button', { name: 'Download', exact: true }).click();
		const result = await downloaded;
		const path = await result.path();
		if (!path) throw new Error('The verified recorder download must produce a local MP4.');
		const bytes = await readFile(path);
		expect(bytes.subarray(4, 8).toString('ascii')).toBe('ftyp');
		expect(bytes.length).toBeGreaterThan(1024);
		await page.getByRole('button', { name: 'Back to comparison' }).click();
		await expect(page.getByRole('combobox', { name: 'Focused camera' })).toHaveValue(second);
		await expect(page.getByRole('combobox', { name: 'Audio source' })).toHaveValue(first);
		const returned = new URL(page.url()).searchParams;
		expect(returned.getAll('compare')).toEqual([...cardSourceIds]);
		expect([returned.get('at'), returned.get('rangeStart'), returned.get('rangeEnd')]).toEqual([
			String(at + 10000),
			String(at),
			String(at + 5000)
		]);
		const stale = new URL(exportUrl);
		stale.searchParams.set('rangeStart', String(at - 80_000));
		await page.goto(stale.href);
		await expect(page.getByRole('alert')).toHaveText(/no longer covers the entire selected range/);
		await expect(panel).toHaveCount(0);
	} finally {
		try {
			await context.unroute('**/*', redirect);
		} finally {
			await server.close();
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
