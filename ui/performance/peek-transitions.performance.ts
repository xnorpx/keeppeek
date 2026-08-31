import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { expect, test, type Locator, type Page } from '@playwright/test';
import {
	nineCameraCircularStartSeparationSeconds,
	nineCameraMinimumStartSeparationSeconds
} from '../src/lib/server/storybook/nine-camera-fixture';

type CameraDraft = {
	id: string;
	name: string;
	startAtSeconds: number;
	profiles: Array<{
		stream: 'main' | 'sub';
		codec: 'h264' | 'h265';
		width: number;
		height: number;
	}>;
};

type CameraDrafts = {
	selection: { sourceDurationSeconds: number; minimumStartSeparationSeconds: number };
	cameras: CameraDraft[];
};

type CoverageSample = {
	atMs: number;
	covered: boolean;
	destinationFrame: boolean;
	surface: 'focus' | 'none' | 'wall';
	cachedTileCount: number;
	transitionFrame: boolean;
};

type CoverageResult = {
	destinationFirstFrameMs: number | null;
	longestUncoveredMs: number;
	samples: number;
	maximumCachedTileCount: number;
	transitionFrameSamples: number;
};

type TransitionResult = CoverageResult & {
	cameraId: string;
	cameraName: string;
	direction: 'dashboard-to-focus' | 'focus-to-dashboard';
	settledStream?: string | null;
	settledCodec?: string | null;
	expectedCodec?: string;
	settledDimensions?: string;
	settledMs?: number;
};

function positiveNumberEnvironment(name: string, fallback: number): number {
	const value = Number(process.env[name] ?? fallback);
	if (!Number.isFinite(value) || value < 0) throw new Error(`${name} must not be negative`);
	return value;
}

const firstFrameBudgetMs = positiveNumberEnvironment('KEEPPEEK_PEEK_FIRST_FRAME_BUDGET_MS', 10_000);
const allWallFramesBudgetMs = positiveNumberEnvironment(
	'KEEPPEEK_PEEK_ALL_WALL_FRAMES_BUDGET_MS',
	5_000
);
const warmDashboardFirstFrameBudgetMs = positiveNumberEnvironment(
	'KEEPPEEK_PEEK_WARM_DASHBOARD_FIRST_FRAME_BUDGET_MS',
	1_000
);
const streamSettleBudgetMs = positiveNumberEnvironment(
	'KEEPPEEK_PEEK_STREAM_SETTLE_BUDGET_MS',
	10_000
);
const uncoveredBudgetMs = positiveNumberEnvironment('KEEPPEEK_PEEK_UNCOVERED_BUDGET_MS', 100);
const luminanceFloor = 8;
const initialHardwareConcurrency = 18;
const initialWallTimeoutMs = 90_000;
const focusLoopCount = 3;
const draftsPath = resolve('../target/nine-camera-demo/camera-drafts.json');
const reportPath = resolve('../target/peek-performance/transitions/latest.json');

function wallVideo(page: Page, cameraId: string): Locator {
	return page.locator(`[data-peek-camera="${cameraId}"] video`);
}

function focusView(page: Page, cameraId: string): Locator {
	return page.locator(`[data-peek-focus-stage] [data-camera-id="${cameraId}"]`);
}

async function videoLuminance(video: Locator): Promise<number> {
	return video.evaluate((element) => {
		if (
			!(element instanceof HTMLVideoElement) ||
			element.videoWidth <= 0 ||
			element.readyState < 2
		) {
			return 0;
		}
		const canvas = document.createElement('canvas');
		canvas.width = 16;
		canvas.height = 9;
		const context = canvas.getContext('2d', { willReadFrequently: true });
		if (!context) return 0;
		try {
			context.drawImage(element, 0, 0, canvas.width, canvas.height);
			const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
			let total = 0;
			for (let index = 0; index < pixels.length; index += 4) {
				total += pixels[index] + pixels[index + 1] + pixels[index + 2];
			}
			return total / (canvas.width * canvas.height * 3);
		} catch {
			return 0;
		}
	});
}

async function waitForNonBlackVideo(video: Locator, timeoutMs: number): Promise<void> {
	await expect
		.poll(() => videoLuminance(video), { timeout: timeoutMs, intervals: [16, 25, 50, 100] })
		.toBeGreaterThan(luminanceFloor);
}

async function waitForNonBlackWall(
	page: Page,
	cameras: readonly CameraDraft[],
	timeoutMs: number
): Promise<void> {
	await Promise.all(
		cameras.map((camera) => waitForNonBlackVideo(wallVideo(page, camera.id), timeoutMs))
	);
}

async function startCoverageSampling(page: Page, expectedWallVideos: number): Promise<void> {
	await page.evaluate(
		({ expectedWallVideos, luminanceFloor }) => {
			type SamplerState = { active: boolean; startedAtMs: number; samples: CoverageSample[] };
			const samplerWindow = window as Window & { __peekCoverageSampler?: SamplerState };
			const state: SamplerState = {
				active: true,
				startedAtMs: performance.now(),
				samples: []
			};
			samplerWindow.__peekCoverageSampler = state;
			const canvas = document.createElement('canvas');
			canvas.width = 16;
			canvas.height = 9;
			const context = canvas.getContext('2d', { willReadFrequently: true });
			const nonBlack = (source: HTMLImageElement | HTMLVideoElement | null): boolean => {
				if (!source || !context) return false;
				if (
					(source instanceof HTMLVideoElement &&
						(source.videoWidth <= 0 || source.readyState < 2)) ||
					(source instanceof HTMLImageElement && (!source.complete || source.naturalWidth <= 0))
				) {
					return false;
				}
				try {
					context.drawImage(source, 0, 0, canvas.width, canvas.height);
					const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
					let total = 0;
					for (let index = 0; index < pixels.length; index += 4) {
						total += pixels[index] + pixels[index + 1] + pixels[index + 2];
					}
					return total / (canvas.width * canvas.height * 3) > luminanceFloor;
				} catch {
					return false;
				}
			};
			const sample = () => {
				if (!state.active) return;
				const focusStage = document.querySelector<HTMLElement>('[data-peek-focus-stage]');
				const focus = focusStage?.querySelector<HTMLVideoElement>('video') ?? null;
				const focusFallback =
					focusStage?.querySelector<HTMLImageElement>('[data-peek-cached-frame]') ?? null;
				const wall = [...document.querySelectorAll<HTMLElement>('[data-peek-camera]')];
				const transition = document.querySelector<HTMLImageElement>('[data-peek-transition-frame]');
				const surface =
					location.pathname === '/viewer' && focus ? 'focus' : wall.length > 0 ? 'wall' : 'none';
				const cachedTileCount = wall.filter((tile) =>
					nonBlack(tile.querySelector<HTMLImageElement>('[data-peek-cached-frame]'))
				).length;
				const destinationFrame =
					surface === 'focus'
						? nonBlack(focus) || nonBlack(focusFallback)
						: wall.length === expectedWallVideos &&
							wall.every(
								(tile) =>
									nonBlack(tile.querySelector<HTMLVideoElement>('video')) ||
									nonBlack(tile.querySelector<HTMLImageElement>('[data-peek-cached-frame]'))
							);
				const transitionFrame = nonBlack(transition);
				state.samples.push({
					atMs: performance.now(),
					covered: destinationFrame || transitionFrame,
					destinationFrame,
					surface,
					cachedTileCount,
					transitionFrame
				});
				requestAnimationFrame(sample);
			};
			requestAnimationFrame(sample);
		},
		{ expectedWallVideos, luminanceFloor }
	);
}

async function stopCoverageSampling(
	page: Page,
	destination: 'focus' | 'wall'
): Promise<CoverageResult> {
	await page.evaluate(
		() => new Promise<void>((resolveFrame) => requestAnimationFrame(() => resolveFrame()))
	);
	return page.evaluate((destination) => {
		type SamplerState = { active: boolean; startedAtMs: number; samples: CoverageSample[] };
		const samplerWindow = window as Window & { __peekCoverageSampler?: SamplerState };
		const state = samplerWindow.__peekCoverageSampler;
		if (!state) throw new Error('Peek transition coverage sampler is unavailable');
		state.active = false;
		const firstDestinationFrame = state.samples.find(
			(sample) => sample.surface === destination && sample.destinationFrame
		);
		let longestUncoveredMs = 0;
		let uncoveredAtMs: number | null = null;
		for (const sample of state.samples) {
			if (!sample.covered && uncoveredAtMs === null) uncoveredAtMs = sample.atMs;
			if (sample.covered && uncoveredAtMs !== null) {
				longestUncoveredMs = Math.max(longestUncoveredMs, sample.atMs - uncoveredAtMs);
				uncoveredAtMs = null;
			}
		}
		return {
			destinationFirstFrameMs:
				firstDestinationFrame === undefined ? null : firstDestinationFrame.atMs - state.startedAtMs,
			longestUncoveredMs,
			samples: state.samples.length,
			maximumCachedTileCount: Math.max(0, ...state.samples.map((sample) => sample.cachedTileCount)),
			transitionFrameSamples: state.samples.filter((sample) => sample.transitionFrame).length
		};
	}, destination);
}

function percentile(values: readonly number[], fraction: number): number {
	const ordered = values.toSorted((left, right) => left - right);
	return ordered[Math.max(0, Math.ceil(ordered.length * fraction) - 1)] ?? 0;
}

function rounded(value: number): number {
	return Number(value.toFixed(2));
}

test('loops H.264 and H.265 focus over the nine-camera wall within frame budgets', async ({
	page
}) => {
	const browserErrors: string[] = [];
	page.on('console', (message) => {
		const text = message.text();
		const missingFavicon =
			message.type() === 'error' &&
			text === 'Failed to load resource: the server responded with a status of 404 (Not Found)' &&
			message.location().url.endsWith('/favicon.png');
		if (
			(!missingFavicon && message.type() === 'error') ||
			(message.type() === 'warning' && text.includes('[svelte]'))
		) {
			browserErrors.push(text);
		}
	});
	page.on('pageerror', (error) => browserErrors.push(error.message));
	const drafts = JSON.parse(await readFile(draftsPath, 'utf8')) as CameraDrafts;
	expect(drafts.cameras).toHaveLength(9);
	expect(drafts.selection.minimumStartSeparationSeconds).toBe(
		nineCameraMinimumStartSeparationSeconds
	);
	expect(
		nineCameraCircularStartSeparationSeconds(
			drafts.cameras.map((camera) => camera.startAtSeconds),
			drafts.selection.sourceDurationSeconds
		)
	).toBeGreaterThanOrEqual(drafts.selection.minimumStartSeparationSeconds);

	await page.addInitScript((value) => {
		Object.defineProperty(navigator, 'hardwareConcurrency', { configurable: true, value });
	}, initialHardwareConcurrency);
	await page.goto('/');
	await expect(page.locator('[data-peek-camera]')).toHaveCount(drafts.cameras.length);
	await waitForNonBlackWall(page, drafts.cameras, initialWallTimeoutMs);
	const supportsH265 = await page.evaluate(
		() =>
			RTCRtpReceiver.getCapabilities?.('video')?.codecs.some(
				(codec) => codec.mimeType.toLowerCase() === 'video/h265'
			) ?? false
	);
	const initialSessionId = await page
		.locator('[data-peek-camera] [data-session-id]')
		.first()
		.getAttribute('data-session-id');
	expect(initialSessionId).not.toBeNull();
	const peekView = page.locator('[data-peek-view]');
	await peekView.evaluate((element) => (element.dataset.peekInstance = 'performance'));

	const transitions: TransitionResult[] = [];
	const focusCameras = drafts.cameras.slice(0, 2);
	for (const camera of Array.from({ length: focusLoopCount }, () => focusCameras).flat()) {
		await startCoverageSampling(page, drafts.cameras.length);
		await page.getByRole('button', { name: `Focus ${camera.name} live view` }).click();
		await expect(page).toHaveURL(
			new RegExp(`/viewer\\?camera=${camera.id.replaceAll('.', '\\.')}$`)
		);
		await expect(peekView).toHaveAttribute('data-peek-instance', 'performance');
		const focused = focusView(page, camera.id);
		await waitForNonBlackVideo(focused.locator('video'), firstFrameBudgetMs);
		expect(await focused.getAttribute('data-session-id')).toBe(initialSessionId);

		const expectedProfile =
			camera.profiles.find((profile) => profile.stream === 'main' && profile.codec !== 'h265') ??
			(supportsH265 ? camera.profiles.find((profile) => profile.stream === 'main') : undefined) ??
			camera.profiles.find((profile) => profile.codec === 'h264') ??
			camera.profiles[0]!;
		const settleStartedAt = performance.now();
		await expect(focused).toHaveAttribute('data-requested-variant', expectedProfile.stream, {
			timeout: streamSettleBudgetMs
		});
		await expect
			.poll(() => focused.getAttribute('data-pending-stream'), { timeout: streamSettleBudgetMs })
			.toBeNull();
		await waitForNonBlackVideo(focused.locator('video'), firstFrameBudgetMs);
		await expect(focused).toHaveAttribute('data-stream', expectedProfile.stream);
		const expectedDimensions = `${expectedProfile.width}x${expectedProfile.height}`;
		await expect
			.poll(
				() =>
					focused.locator('video').evaluate((video) => `${video.videoWidth}x${video.videoHeight}`),
				{ timeout: streamSettleBudgetMs }
			)
			.toBe(expectedDimensions);
		const focusCoverage = await stopCoverageSampling(page, 'focus');
		expect(focusCoverage.destinationFirstFrameMs).not.toBeNull();
		expect(focusCoverage.destinationFirstFrameMs!).toBeLessThanOrEqual(firstFrameBudgetMs);
		expect(focusCoverage.longestUncoveredMs).toBeLessThanOrEqual(uncoveredBudgetMs);
		transitions.push({
			cameraId: camera.id,
			cameraName: camera.name,
			direction: 'dashboard-to-focus',
			...focusCoverage,
			settledStream: await focused.getAttribute('data-stream'),
			settledCodec: await focused.getAttribute('data-codec'),
			expectedCodec: `video/${expectedProfile.codec}`,
			settledDimensions: expectedDimensions,
			settledMs: performance.now() - settleStartedAt
		});

		await startCoverageSampling(page, drafts.cameras.length);
		await page.getByRole('link', { name: 'Dashboard', exact: true }).click();
		await expect(page).toHaveURL(/\/$/);
		await expect(peekView).toHaveAttribute('data-peek-instance', 'performance');
		await waitForNonBlackWall(page, drafts.cameras, allWallFramesBudgetMs);
		const dashboardCoverage = await stopCoverageSampling(page, 'wall');
		expect(dashboardCoverage.destinationFirstFrameMs).not.toBeNull();
		expect(dashboardCoverage.destinationFirstFrameMs!).toBeLessThanOrEqual(
			warmDashboardFirstFrameBudgetMs
		);
		expect(dashboardCoverage.longestUncoveredMs).toBeLessThanOrEqual(uncoveredBudgetMs);
		expect(dashboardCoverage.transitionFrameSamples).toBe(0);
		await expect(page.locator('[data-peek-wall]')).toHaveAttribute(
			'data-peek-wall-target-count',
			'9'
		);
		expect(
			await page
				.locator('[data-peek-camera] [data-session-id]')
				.first()
				.getAttribute('data-session-id')
		).toBe(initialSessionId);
		transitions.push({
			cameraId: camera.id,
			cameraName: camera.name,
			direction: 'focus-to-dashboard',
			...dashboardCoverage
		});
	}

	const firstFrameDurations = transitions.flatMap((transition) =>
		transition.destinationFirstFrameMs === null ? [] : [transition.destinationFirstFrameMs]
	);
	const uncoveredDurations = transitions.map((transition) => transition.longestUncoveredMs);
	const warmDashboardDurations = transitions.flatMap((transition) =>
		transition.direction === 'focus-to-dashboard' && transition.destinationFirstFrameMs !== null
			? [transition.destinationFirstFrameMs]
			: []
	);
	if (supportsH265) {
		expect(
			transitions.some(
				(transition) =>
					transition.direction === 'dashboard-to-focus' &&
					transition.expectedCodec === 'video/h265' &&
					transition.settledStream === 'main'
			)
		).toBe(true);
	}
	expect(transitions.every((transition) => transition.transitionFrameSamples === 0)).toBe(true);
	expect(browserErrors).toEqual([]);
	const report = {
		schemaVersion: 1,
		generatedAt: new Date().toISOString(),
		budgets: {
			firstFrameBudgetMs,
			allWallFramesBudgetMs,
			warmDashboardFirstFrameBudgetMs,
			streamSettleBudgetMs,
			uncoveredBudgetMs,
			luminanceFloor
		},
		sourceDurationSeconds: drafts.selection.sourceDurationSeconds,
		supportsH265,
		cameraStarts: drafts.cameras.map(({ id, name, startAtSeconds }) => ({
			id,
			name,
			startAtSeconds
		})),
		summary: {
			firstFrameP50Ms: rounded(percentile(firstFrameDurations, 0.5)),
			firstFrameP95Ms: rounded(percentile(firstFrameDurations, 0.95)),
			warmDashboardFirstFrameP50Ms: rounded(percentile(warmDashboardDurations, 0.5)),
			warmDashboardFirstFrameP95Ms: rounded(percentile(warmDashboardDurations, 0.95)),
			uncoveredP50Ms: rounded(percentile(uncoveredDurations, 0.5)),
			uncoveredP95Ms: rounded(percentile(uncoveredDurations, 0.95))
		},
		transitions: transitions.map((transition) => ({
			...transition,
			destinationFirstFrameMs:
				transition.destinationFirstFrameMs === null
					? null
					: rounded(transition.destinationFirstFrameMs),
			longestUncoveredMs: rounded(transition.longestUncoveredMs),
			settledMs: transition.settledMs === undefined ? undefined : rounded(transition.settledMs)
		}))
	};
	await mkdir(resolve(reportPath, '..'), { recursive: true });
	await writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`);
	console.log(JSON.stringify(report.summary));
});
