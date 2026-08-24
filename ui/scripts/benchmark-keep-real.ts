import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { chromium, type Page } from '@playwright/test';

type TimelinePerformanceEvent = {
	name: string;
	atMs: number;
	durationMs?: number;
	sourceId?: string;
	startMs?: number;
	endMs?: number;
};

type KeepQaState = {
	events: TimelinePerformanceEvent[];
	longTasks: Array<{ startTime: number; duration: number }>;
};

declare global {
	interface Window {
		__keeppeekQa: KeepQaState;
	}
}

type CameraOption = {
	label: string;
	value: string;
};

type VideoSample = {
	currentTime: number;
	totalVideoFrames: number;
	droppedVideoFrames: number;
};

function argument(name: string, fallback: string): string {
	const index = process.argv.indexOf(`--${name}`);
	return index >= 0 && process.argv[index + 1] ? process.argv[index + 1]! : fallback;
}

function positiveNumberArgument(name: string, fallback: number): number {
	const value = Number(argument(name, fallback.toString()));
	if (!Number.isFinite(value) || value <= 0) throw new Error(`--${name} must be positive`);
	return value;
}

async function installCollectors(page: Page): Promise<void> {
	await page.addInitScript(() => {
		window.__keeppeekQa = { events: [], longTasks: [] };
		window.addEventListener('keeppeek:timeline-performance', (event) => {
			window.__keeppeekQa.events.push((event as CustomEvent<TimelinePerformanceEvent>).detail);
		});
		try {
			new PerformanceObserver((list) => {
				window.__keeppeekQa.longTasks.push(
					...list.getEntries().map((entry) => ({
						startTime: entry.startTime,
						duration: entry.duration
					}))
				);
			}).observe({ entryTypes: ['longtask'] });
		} catch {}
	});
}

async function videoSample(page: Page): Promise<VideoSample> {
	return page.evaluate(() => {
		const video = document.querySelector('video');
		const quality = video?.getVideoPlaybackQuality?.();
		return {
			currentTime: video?.currentTime ?? 0,
			totalVideoFrames: quality?.totalVideoFrames ?? 0,
			droppedVideoFrames: quality?.droppedVideoFrames ?? 0
		};
	});
}

const baseUrl = argument('url', process.env.KEEPPEEK_URL ?? 'http://127.0.0.1:3000');
const date = argument('date', new Date().toISOString().slice(0, 10));
const stream = argument('stream', 'sub');
const at = argument('at', '');
const sampleMs = positiveNumberArgument('sample-ms', 5_000);
const timeoutMs = positiveNumberArgument('timeout-ms', 10_000);
const requestedCameraIds = argument('cameras', '')
	.split(',')
	.map((cameraId) => cameraId.trim())
	.filter(Boolean);
const outputPath = resolve(argument('output', '../target/keep-performance/real-keep.json'));

if (stream !== 'main' && stream !== 'sub') throw new Error('--stream must be main or sub');

const browser = await chromium.launch({ headless: true });
const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });

try {
	const catalogPage = await context.newPage();
	const catalogUrl = new URL('/keep', baseUrl);
	catalogUrl.searchParams.set('stream', stream);
	catalogUrl.searchParams.set('date', date);
	await catalogPage.goto(catalogUrl.href, { waitUntil: 'domcontentloaded' });
	const cameraSelect = catalogPage.getByRole('combobox', { name: 'Camera' });
	await cameraSelect.waitFor({ state: 'visible', timeout: timeoutMs });
	await cameraSelect.locator('option').first().waitFor({ state: 'attached', timeout: timeoutMs });
	const allCameras = await cameraSelect.locator('option').evaluateAll((options) =>
		options.map((option) => {
			const cameraOption = option as HTMLOptionElement;
			return {
				label: cameraOption.textContent?.trim() || cameraOption.value,
				value: cameraOption.value
			};
		})
	);
	await catalogPage.close();

	const cameras: CameraOption[] =
		requestedCameraIds.length === 0
			? allCameras
			: allCameras.filter((camera) => requestedCameraIds.includes(camera.value));
	if (cameras.length === 0) throw new Error('No matching cameras were reported by KeepPeek');

	const results = [];
	for (const camera of cameras) {
		const page = await context.newPage();
		const consoleErrors: string[] = [];
		const requestFailures: string[] = [];
		page.on('console', (message) => {
			if (message.type() === 'error') consoleErrors.push(message.text());
		});
		page.on('requestfailed', (request) => {
			requestFailures.push(
				`${request.method()} ${request.url()} ${request.failure()?.errorText ?? ''}`
			);
		});
		await installCollectors(page);
		const url = new URL('/keep', baseUrl);
		url.searchParams.set('camera', camera.value);
		url.searchParams.set('stream', stream);
		url.searchParams.set('date', date);
		if (at) {
			const timestampMs = /^\d+$/.test(at) ? Number(at) : Date.parse(at);
			if (!Number.isSafeInteger(timestampMs))
				throw new Error('--at must be an ISO date or epoch ms');
			url.searchParams.set('at', timestampMs.toString());
		}
		await page.goto(url.href, { waitUntil: 'domcontentloaded' });
		await page
			.waitForFunction(
				() => window.__keeppeekQa.events.some((event) => event.name === 'ReplayFirstFrame'),
				undefined,
				{ timeout: timeoutMs }
			)
			.catch(() => undefined);

		const baseline = await videoSample(page);
		await page.waitForTimeout(sampleMs);
		const final = await videoSample(page);
		const details = await page.evaluate((sourceId) => {
			const video = document.querySelector('video');
			const events = window.__keeppeekQa.events;
			const first = (name: string) =>
				events.find((event) => event.name === name && event.sourceId === sourceId)?.atMs ?? null;
			const timelineQueries = events.filter(
				(event) => event.name === 'TimelineQueryStarted' && event.sourceId === sourceId
			);
			return {
				firstFragmentMs: first('ReplayFirstFragment'),
				firstFrameMs: first('ReplayFirstFrame'),
				timelineQueries: timelineQueries.length,
				timelineQuerySpansMinutes: timelineQueries.map(
					(event) => ((event.endMs ?? 0) - (event.startMs ?? 0)) / 60_000
				),
				timelineFirstPageMs: events
					.filter((event) => event.name === 'TimelineFirstPage' && event.sourceId === sourceId)
					.map((event) => event.durationMs ?? null),
				timelineCompletedMs: events
					.filter((event) => event.name === 'TimelineQueryCompleted' && event.sourceId === sourceId)
					.map((event) => event.durationMs ?? null),
				thumbnailSignals: events.filter((event) => event.name.startsWith('Thumbnail')).length,
				refills: events.filter(
					(event) => event.name === 'ReplayRefill' && event.sourceId === sourceId
				).length,
				longTasks: window.__keeppeekQa.longTasks,
				video: video
					? {
							paused: video.paused,
							muted: video.muted,
							readyState: video.readyState,
							width: video.videoWidth,
							height: video.videoHeight,
							error: video.error?.message ?? null
						}
					: null,
				playerError: document.querySelector('[data-keep-player] + p')?.textContent?.trim() ?? null,
				coldSeek: document.querySelector('[data-cold-seek]')?.textContent?.trim() ?? null
			};
		}, camera.value);
		const sampleSeconds = sampleMs / 1_000;
		results.push({
			camera,
			stream,
			date,
			...details,
			steadyPlayback: {
				mediaSecondsAdvanced: final.currentTime - baseline.currentTime,
				presentedFrames: final.totalVideoFrames - baseline.totalVideoFrames,
				droppedFrames: final.droppedVideoFrames - baseline.droppedVideoFrames,
				presentedFps: (final.totalVideoFrames - baseline.totalVideoFrames) / sampleSeconds
			},
			consoleErrors,
			requestFailures
		});
		await page.close();
	}

	const report = {
		schemaVersion: 1,
		generatedAt: new Date().toISOString(),
		baseUrl,
		date,
		stream,
		at: at || null,
		sampleMs,
		timeoutMs,
		results
	};
	await mkdir(dirname(outputPath), { recursive: true });
	await writeFile(outputPath, `${JSON.stringify(report, null, 2)}\n`);
	console.table(
		results.map((result) => ({
			camera: result.camera.label,
			firstFragmentMs: result.firstFragmentMs,
			firstFrameMs: result.firstFrameMs,
			fps: result.steadyPlayback.presentedFps.toFixed(1),
			mediaSeconds: result.steadyPlayback.mediaSecondsAdvanced.toFixed(2),
			paused: result.video?.paused,
			longTaskMaxMs: Math.max(0, ...result.longTasks.map((task) => task.duration)).toFixed(1),
			errors: result.consoleErrors.length + result.requestFailures.length
		}))
	);
	console.log(`Wrote ${outputPath}`);
} finally {
	await context.close();
	await browser.close();
}
