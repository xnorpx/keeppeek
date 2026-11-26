import { spawn, type ChildProcess } from 'node:child_process';
import { mkdir, rm, writeFile } from 'node:fs/promises';
import net from 'node:net';
import { arch, cpus, platform, release, totalmem } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { expect, test, type Page } from '@playwright/test';
import {
	requiredReleaseBinary,
	seedPlaybackDatabase,
	type PlaybackCorpusManifest
} from '../scripts/seed-playback-database';

type TimelinePerformanceEvent = {
	name: string;
	atMs: number;
	durationMs?: number;
	sourceId?: string;
	cursorId?: string;
	generation?: string;
	cache?: string;
	startMs?: number;
	endMs?: number;
};

type PlaybackQaState = {
	events: TimelinePerformanceEvent[];
	longTasks: Array<{ startTime: number; duration: number }>;
};

type LatencySummary = {
	samples: number;
	p50Ms: number;
	p95Ms: number;
	maxMs: number;
};

type PlaybackBenchmarkReport = {
	schemaVersion: 1;
	label: string;
	generatedAt: string;
	environment: {
		platform: string;
		release: string;
		arch: string;
		cpus: number;
		memoryGiB: number;
		ci: boolean;
	};
	corpus: {
		date: string;
		cameras: number;
		recordingCount: number;
		fragmentCount: number;
		eventCount: number;
		bunnyEventCount: number;
		thumbnailCount: number;
		seedDurationMs: number;
	};
	budgets: Record<string, number>;
	metrics: {
		keepColdLoad: {
			firstFragmentMs: number;
			firstFrameMs: number;
			timelineEventsReadyMs: number;
			videoWidth: number;
			videoHeight: number;
		};
		scrubbing: {
			cold: LatencySummary;
			revisit: LatencySummary;
			all: LatencySummary;
			memoryCacheHits: number;
			decoderRenders: number;
			commitToFrameMs: number;
		};
		eventHopping: LatencySummary;
		timelineInteractions: LatencySummary & {
			peakTimelineNodes: number;
		};
		eventsBrowse: {
			initialPageMs: number;
			bunnySearchMs: number;
			thumbnailsReadyMs: number;
			thumbnailCount: number;
			zoneFilterMs: number;
			zoneResultCount: number;
			openInKeepFrameMs: number;
		};
		cameraSwitch: {
			switchToFrameMs: number;
		};
		steadyPlayback: {
			sampleMs: number;
			presentedFrames: number;
			droppedFrames: number;
			presentedFps: number;
			mediaSecondsAdvanced: number;
			refills: number;
		};
		maxLongTaskMs: number;
		consoleErrors: number;
		requestFailures: number;
	};
};

declare global {
	interface Window {
		__keeppeekPlaybackPerf: PlaybackQaState;
	}
}

const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const reportDirectory = path.join(repositoryRoot, 'target', 'keep-performance', 'playback');
const reportLabel = (process.env.KEEPPEEK_PLAYBACK_PERF_LABEL ?? 'latest').replaceAll(
	/[^a-z0-9_-]/gi,
	'-'
);
const enforceBudgets = process.env.KEEPPEEK_PLAYBACK_PERF_ENFORCE !== '0';
const keepCorpus = process.env.KEEPPEEK_PLAYBACK_PERF_KEEP_DATA === '1';
const contendedRunner = Boolean(process.env.CI);

// Budgets enforce snappy end-to-end UX while absorbing shared CI runner variance.
const budgets = {
	keepFirstFrameMs: contendedRunner ? 4_000 : 3_000,
	keepTimelineEventsReadyMs: contendedRunner ? 4_500 : 3_500,
	scrubColdP95Ms: contendedRunner ? 400 : 250,
	scrubRevisitP95Ms: contendedRunner ? 150 : 100,
	scrubCommitMs: contendedRunner ? 1_000 : 600,
	eventHopP95Ms: contendedRunner ? 1_200 : 800,
	timelineInteractionP95Ms: contendedRunner ? 250 : 150,
	eventsInitialPageMs: contendedRunner ? 3_500 : 2_500,
	eventsBunnySearchMs: contendedRunner ? 1_200 : 800,
	eventsThumbnailsReadyMs: contendedRunner ? 2_500 : 1_500,
	eventsZoneFilterMs: contendedRunner ? 1_000 : 600,
	eventsToKeepFrameMs: contendedRunner ? 2_500 : 1_500,
	cameraSwitchMs: contendedRunner ? 2_000 : 1_200,
	steadyMinFps: 8,
	steadyMinMediaSeconds: 1.8,
	maxTimelineNodes: 1_600
};

function percentile(values: readonly number[], p: number): number {
	if (values.length === 0) return 0;
	const sorted = values.toSorted((left, right) => left - right);
	return sorted[Math.max(0, Math.ceil(sorted.length * p) - 1)] ?? 0;
}

function rounded(value: number): number {
	return Number(value.toFixed(2));
}

function summarize(values: readonly number[]): LatencySummary {
	return {
		samples: values.length,
		p50Ms: rounded(percentile(values, 0.5)),
		p95Ms: rounded(percentile(values, 0.95)),
		maxMs: rounded(Math.max(0, ...values))
	};
}

async function freePort(): Promise<number> {
	return new Promise((resolve, reject) => {
		const server = net.createServer();
		server.once('error', reject);
		server.listen(0, '127.0.0.1', () => {
			const address = server.address();
			if (!address || typeof address === 'string') {
				server.close(() => reject(new Error('Unable to allocate an ephemeral TCP port')));
				return;
			}
			const { port } = address;
			server.close((error) => (error ? reject(error) : resolve(port)));
		});
	});
}

async function waitForHttpReady(url: string, timeoutMs: number): Promise<void> {
	const deadline = Date.now() + timeoutMs;
	let lastError = 'server did not respond';
	while (Date.now() < deadline) {
		try {
			const response = await fetch(url);
			if (response.ok) return;
			lastError = `HTTP ${response.status}`;
		} catch (error) {
			lastError = error instanceof Error ? error.message : String(error);
		}
		await new Promise((resolve) => setTimeout(resolve, 100));
	}
	throw new Error(`KeepPeek server at ${url} did not become ready: ${lastError}`);
}

async function installCollectors(page: Page): Promise<void> {
	await page.addInitScript(() => {
		window.__keeppeekPlaybackPerf = { events: [], longTasks: [] };
		window.addEventListener('keeppeek:timeline-performance', (event) => {
			window.__keeppeekPlaybackPerf.events.push(
				(event as CustomEvent<TimelinePerformanceEvent>).detail
			);
		});
		try {
			new PerformanceObserver((list) => {
				window.__keeppeekPlaybackPerf.longTasks.push(
					...list.getEntries().map((entry) => ({
						startTime: entry.startTime,
						duration: entry.duration
					}))
				);
			}).observe({ entryTypes: ['longtask'] });
		} catch {
			// Long task observer is optional on runtimes without PerformanceObserver support.
		}
	});
}

async function readEventCount(page: Page, name: string): Promise<number> {
	return page.evaluate(
		(eventName) =>
			window.__keeppeekPlaybackPerf.events.filter((event) => event.name === eventName).length,
		name
	);
}

async function waitForEventCountAtLeast(
	page: Page,
	name: string,
	targetCount: number,
	timeoutMs = 10_000
): Promise<TimelinePerformanceEvent> {
	try {
		const handle = await page.waitForFunction(
			({ eventName, minCount }) => {
				const matching = window.__keeppeekPlaybackPerf.events.filter(
					(event) => event.name === eventName
				);
				return matching.length >= minCount ? matching[minCount - 1] : null;
			},
			{ eventName: name, minCount: targetCount },
			{ timeout: timeoutMs }
		);
		return handle.jsonValue() as Promise<TimelinePerformanceEvent>;
	} catch (error) {
		const diagnostics = await page
			.evaluate(() => ({
				href: window.location.href,
				zoom: document
					.querySelector('[aria-label="Recording timeline"]')
					?.getAttribute('data-timeline-zoom'),
				qualityValue:
					(document.querySelector('[data-keep-quality-control] select') as HTMLSelectElement | null)
						?.value ?? null,
				availabilityBars: [...document.querySelectorAll('[data-timeline-availability]')].map(
					(el) => [el.getAttribute('data-start-ms'), el.getAttribute('data-end-ms')]
				),
				events: window.__keeppeekPlaybackPerf.events,
				playerError: document.querySelector('[data-keep-player] + p')?.textContent?.trim() ?? null,
				statuses: [...document.querySelectorAll('[role="status"]')].map((el) =>
					el.textContent?.trim()
				),
				alerts: [...document.querySelectorAll('[role="alert"]')].map((el) =>
					el.textContent?.trim()
				),
				playheadMs:
					document
						.querySelector('[data-recording-timeline-playhead-ms]')
						?.getAttribute('data-recording-timeline-playhead-ms') ?? null
			}))
			.catch(() => null);
		throw new Error(
			`Timed out waiting for ${name} #${targetCount}: ${error instanceof Error ? error.message : String(error)}\nDiagnostics: ${JSON.stringify(diagnostics)}`
		);
	}
}

async function waitForNextDecodedVideoFrame(
	page: Page,
	previousFrames: number,
	timeoutMs = 10_000
): Promise<number> {
	const handle = await page.waitForFunction(
		(minFrames) => {
			const video = document.querySelector('video');
			if (!video || video.readyState < HTMLMediaElement.HAVE_CURRENT_DATA || video.videoWidth === 0)
				return null;
			const frames = video.getVideoPlaybackQuality?.().totalVideoFrames ?? 0;
			if (frames > minFrames) return frames;
			return null;
		},
		previousFrames,
		{ timeout: timeoutMs }
	);
	return handle.jsonValue() as Promise<number>;
}

async function readVideoQuality(page: Page): Promise<{
	currentTime: number;
	totalVideoFrames: number;
	droppedVideoFrames: number;
	readyState: number;
	width: number;
	height: number;
	paused: boolean;
}> {
	return page.evaluate(() => {
		const video = document.querySelector('video');
		const quality = video?.getVideoPlaybackQuality?.();
		return {
			currentTime: video?.currentTime ?? 0,
			totalVideoFrames: quality?.totalVideoFrames ?? 0,
			droppedVideoFrames: quality?.droppedVideoFrames ?? 0,
			readyState: video?.readyState ?? 0,
			width: video?.videoWidth ?? 0,
			height: video?.videoHeight ?? 0,
			paused: video?.paused ?? true
		};
	});
}

function formatMarkdownReport(report: PlaybackBenchmarkReport): string {
	const { metrics, corpus } = report;
	const rows = [
		['Keep cold first fragment', `${metrics.keepColdLoad.firstFragmentMs} ms`, 'n/a'],
		[
			'Keep cold first frame',
			`${metrics.keepColdLoad.firstFrameMs} ms`,
			`${report.budgets.keepFirstFrameMs} ms`
		],
		[
			'Keep dense timeline events ready',
			`${metrics.keepColdLoad.timelineEventsReadyMs} ms`,
			`${report.budgets.keepTimelineEventsReadyMs} ms`
		],
		[
			'Scrub preview (cold p50 / p95 / max)',
			`${metrics.scrubbing.cold.p50Ms} / ${metrics.scrubbing.cold.p95Ms} / ${metrics.scrubbing.cold.maxMs} ms`,
			`p95 <= ${report.budgets.scrubColdP95Ms} ms`
		],
		[
			'Scrub preview (revisit p50 / p95 / max)',
			`${metrics.scrubbing.revisit.p50Ms} / ${metrics.scrubbing.revisit.p95Ms} / ${metrics.scrubbing.revisit.maxMs} ms (cache hits: ${metrics.scrubbing.memoryCacheHits})`,
			`p95 <= ${report.budgets.scrubRevisitP95Ms} ms`
		],
		[
			'Scrub release -> playing frame',
			`${metrics.scrubbing.commitToFrameMs} ms`,
			`${report.budgets.scrubCommitMs} ms`
		],
		[
			'Timeline event hop -> frame (p50 / p95 / max)',
			`${metrics.eventHopping.p50Ms} / ${metrics.eventHopping.p95Ms} / ${metrics.eventHopping.maxMs} ms`,
			`p95 <= ${report.budgets.eventHopP95Ms} ms`
		],
		[
			'Timeline zoom & filter (p50 / p95 / max)',
			`${metrics.timelineInteractions.p50Ms} / ${metrics.timelineInteractions.p95Ms} / ${metrics.timelineInteractions.maxMs} ms (peak nodes: ${metrics.timelineInteractions.peakTimelineNodes})`,
			`p95 <= ${report.budgets.timelineInteractionP95Ms} ms`
		],
		[
			'Events initial page',
			`${metrics.eventsBrowse.initialPageMs} ms`,
			`${report.budgets.eventsInitialPageMs} ms`
		],
		[
			'Events "bunny" search',
			`${metrics.eventsBrowse.bunnySearchMs} ms`,
			`${report.budgets.eventsBunnySearchMs} ms`
		],
		[
			`Events bunny thumbnails (${metrics.eventsBrowse.thumbnailCount} loaded)`,
			`${metrics.eventsBrowse.thumbnailsReadyMs} ms`,
			`${report.budgets.eventsThumbnailsReadyMs} ms`
		],
		[
			`Events place filter (${metrics.eventsBrowse.zoneResultCount} cards)`,
			`${metrics.eventsBrowse.zoneFilterMs} ms`,
			`${report.budgets.eventsZoneFilterMs} ms`
		],
		[
			'Events detail -> Open in Keep first frame',
			`${metrics.eventsBrowse.openInKeepFrameMs} ms`,
			`${report.budgets.eventsToKeepFrameMs} ms`
		],
		[
			'Camera switch -> first frame',
			`${metrics.cameraSwitch.switchToFrameMs} ms`,
			`${report.budgets.cameraSwitchMs} ms`
		],
		[
			'Steady playback FPS / advanced / dropped',
			`${metrics.steadyPlayback.presentedFps} fps / ${metrics.steadyPlayback.mediaSecondsAdvanced}s / ${metrics.steadyPlayback.droppedFrames} dropped (${metrics.steadyPlayback.refills} refills)`,
			`>= ${report.budgets.steadyMinFps} fps`
		],
		['Peak long task', `${metrics.maxLongTaskMs} ms`, 'diagnostic'],
		['Console & network errors', `${metrics.consoleErrors + metrics.requestFailures}`, '0']
	];
	return [
		`# Playback performance benchmark (${report.label})`,
		'',
		`Generated: ${report.generatedAt}  `,
		`Corpus: ${corpus.cameras} cameras, ${corpus.recordingCount} recordings, ${corpus.fragmentCount} H.264 fragments, ${corpus.eventCount} events (${corpus.bunnyEventCount} bunnies, ${corpus.thumbnailCount} thumbnails), seeded in ${corpus.seedDurationMs} ms.`,
		'',
		'| Metric | Observed | Budget |',
		'| --- | --- | --- |',
		...rows.map(([metric, observed, budget]) => `| ${metric} | ${observed} | ${budget} |`),
		''
	].join('\n');
}

test.describe.serial('Real-database playback and timeline performance', () => {
	let port = 0;
	let baseUrl = '';
	let rootDir = '';
	let manifest!: PlaybackCorpusManifest;
	let serverProcess: ChildProcess | null = null;
	let serverStderr = '';

	test.beforeAll(async () => {
		port = await freePort();
		baseUrl = `http://127.0.0.1:${port}`;
		const seeded = await seedPlaybackDatabase({ port });
		rootDir = seeded.rootDir;
		manifest = seeded.manifest;

		const keeppeekBinary = requiredReleaseBinary('keeppeek');
		serverProcess = spawn(keeppeekBinary, [`--config=${seeded.configPath}`], {
			cwd: repositoryRoot,
			env: { ...process.env, RUST_LOG: 'warn' },
			stdio: ['ignore', 'ignore', 'pipe'],
			windowsHide: true
		});
		serverProcess.stderr?.setEncoding('utf8');
		serverProcess.stderr?.on('data', (chunk: string) => {
			if (serverStderr.length < 16_384) serverStderr += chunk;
		});
		await waitForHttpReady(`${baseUrl}/health`, 20_000);
	});

	test.afterAll(async () => {
		if (serverProcess && serverProcess.exitCode === null) {
			serverProcess.kill('SIGINT');
			await Promise.race([
				new Promise<void>((resolve) => serverProcess?.once('exit', () => resolve())),
				new Promise<void>((resolve) => setTimeout(resolve, 5_000))
			]);
			if (serverProcess.exitCode === null) serverProcess.kill('SIGKILL');
		}
		if (!keepCorpus && rootDir) {
			await rm(rootDir, { recursive: true, force: true }).catch(() => undefined);
		}
	});

	test('measures end-to-end playback, scrubbing, timeline, and bunny event performance', async ({
		browser
	}) => {
		const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
		const page = await context.newPage();
		const consoleErrors: string[] = [];
		const requestFailures: string[] = [];
		const longTaskSamples: number[] = [];

		page.on('pageerror', (error) => {
			consoleErrors.push(`pageerror: ${error.stack ?? error.message}`);
			console.error('PAGEERROR:', error.stack ?? error.message);
		});
		page.on('console', (message) => {
			if (message.type() === 'error') {
				consoleErrors.push(message.text());
				console.error('CONSOLE ERROR:', message.text());
			}
		});
		page.on('requestfailed', (request) => {
			const errorText = request.failure()?.errorText ?? '';
			if (!errorText.includes('ERR_ABORTED')) {
				requestFailures.push(`${request.method()} ${request.url()} ${errorText}`);
			}
		});

		await installCollectors(page);
		const recordPageLongTasks = async () => {
			const entries = await page.evaluate(() =>
				window.__keeppeekPlaybackPerf.longTasks.map((entry) => entry.duration)
			);
			longTaskSamples.push(...entries);
		};

		try {
			const primaryCamera = manifest.cameras[0]!;
			const secondaryCamera = manifest.cameras[1]!;

			// 1. Cold Keep load -> first decoded video frame & dense timeline events ready.
			const keepUrl = `${baseUrl}/keep?camera=${encodeURIComponent(primaryCamera.id)}&stream=sub&date=${manifest.date}&at=${manifest.heroSegment.startMs}`;
			const coldStart = performance.now();
			await page.goto(keepUrl, { waitUntil: 'domcontentloaded' });
			await waitForEventCountAtLeast(page, 'ReplayFirstFrame', 1, 15_000);
			await page
				.locator('[data-timeline-event]')
				.first()
				.waitFor({ state: 'visible', timeout: 15_000 });
			const timelineEventsReadyMs = rounded(performance.now() - coldStart);
			const coldDetails = await page.evaluate(() => {
				const events = window.__keeppeekPlaybackPerf.events;
				const firstFragment =
					events.find((event) => event.name === 'ReplayFirstFragment')?.atMs ?? 0;
				const firstFrame = events.find((event) => event.name === 'ReplayFirstFrame')?.atMs ?? 0;
				const video = document.querySelector('video');
				return {
					firstFragmentMs: firstFragment,
					firstFrameMs: firstFrame,
					videoWidth: video?.videoWidth ?? 0,
					videoHeight: video?.videoHeight ?? 0,
					readyState: video?.readyState ?? 0
				};
			});
			expect(coldDetails.videoWidth).toBe(640);
			expect(coldDetails.videoHeight).toBe(360);
			expect(coldDetails.readyState).toBeGreaterThanOrEqual(2);

			// 2. Rapid timeline scrubbing across hours (cold + revisit) and scrub commit.
			const timeline = page.locator('[aria-label="Recording timeline"]');
			const zoomOutButton = timeline.getByRole('button', { name: 'Zoom timeline out' });
			for (let step = 0; step < 6; step += 1) {
				const zoomLabel = await timeline.getAttribute('data-timeline-zoom');
				if (zoomLabel === '24h') break;
				if (await zoomOutButton.isDisabled()) break;
				await zoomOutButton.click();
			}
			await expect(timeline).toHaveAttribute('data-timeline-zoom', '24h');
			await expect
				.poll(() => timeline.locator('[data-timeline-availability]').count())
				.toBeGreaterThanOrEqual(10);

			const availabilityBars = await timeline
				.locator('[data-timeline-availability]')
				.evaluateAll((elements) => {
					const scrollerRect = document
						.querySelector('[aria-label="Recording timeline scroll viewport"]')
						?.getBoundingClientRect();
					return elements
						.map((element) => {
							const rect = element.getBoundingClientRect();
							return {
								startMs: Number(element.getAttribute('data-start-ms') ?? 0),
								x: rect.left + rect.width / 2,
								y: rect.top + rect.height / 2,
								height: rect.height
							};
						})
						.filter(
							(bar) =>
								bar.height > 0 &&
								bar.y > (scrollerRect ? scrollerRect.top + 8 : 0) &&
								bar.y < (scrollerRect ? scrollerRect.bottom - 8 : 900)
						);
				});
			expect(availabilityBars.length).toBeGreaterThanOrEqual(10);

			// Choose 10 evenly spaced hours across the day, then sweep back across 6 of them.
			const coldBarIndices = Array.from({ length: 10 }, (_, index) =>
				Math.min(
					availabilityBars.length - 1,
					Math.floor((index * (availabilityBars.length - 1)) / 9)
				)
			);
			const revisitBarIndices = [
				coldBarIndices[7]!,
				coldBarIndices[5]!,
				coldBarIndices[3]!,
				coldBarIndices[1]!,
				coldBarIndices[4]!,
				coldBarIndices[6]!
			];

			const playheadHandle = timeline.locator('[data-recording-timeline-playhead-ms]');
			await playheadHandle.waitFor({ state: 'visible' });
			const playheadBox = await playheadHandle.boundingBox();
			if (!playheadBox) throw new Error('Timeline playhead handle has no bounding box');

			const scrubStartIndex = await readEventCount(page, 'ScrubPreviewRendered');
			let renderedCount = scrubStartIndex;
			const coldScrubLatencies: number[] = [];
			const revisitScrubLatencies: number[] = [];

			await page.mouse.move(
				playheadBox.x + playheadBox.width / 2,
				playheadBox.y + playheadBox.height / 2
			);
			await page.mouse.down();
			// Wait for initial scrub preview on pointer down if triggered.
			await page.waitForTimeout(120);
			renderedCount = await readEventCount(page, 'ScrubPreviewRendered');

			for (const barIndex of coldBarIndices) {
				const bar = availabilityBars[barIndex]!;
				const beforeCount = renderedCount;
				const started = performance.now();
				await page.mouse.move(playheadBox.x + playheadBox.width / 2, bar.y, { steps: 1 });
				await waitForEventCountAtLeast(page, 'ScrubPreviewRendered', beforeCount + 1, 5_000);
				coldScrubLatencies.push(performance.now() - started);
				renderedCount = await readEventCount(page, 'ScrubPreviewRendered');
			}

			for (const barIndex of revisitBarIndices) {
				const bar = availabilityBars[barIndex]!;
				const beforeCount = renderedCount;
				const started = performance.now();
				await page.mouse.move(playheadBox.x + playheadBox.width / 2, bar.y, { steps: 1 });
				await waitForEventCountAtLeast(page, 'ScrubPreviewRendered', beforeCount + 1, 5_000);
				revisitScrubLatencies.push(performance.now() - started);
				renderedCount = await readEventCount(page, 'ScrubPreviewRendered');
			}

			const scrubRenderStats = await page.evaluate((startIndex) => {
				const scrubEvents = window.__keeppeekPlaybackPerf.events
					.filter((event) => event.name === 'ScrubPreviewRendered')
					.slice(startIndex);
				return {
					memoryCacheHits: scrubEvents.filter((event) => event.cache === 'memory').length,
					decoderRenders: scrubEvents.filter((event) => event.cache === 'decoder').length
				};
			}, scrubStartIndex);

			const framesBeforeRelease = await readEventCount(page, 'ReplayFirstFrame');
			const commitStart = performance.now();
			await page.mouse.up();
			await waitForEventCountAtLeast(page, 'ReplayFirstFrame', framesBeforeRelease + 1, 10_000);
			const scrubCommitToFrameMs = rounded(performance.now() - commitStart);

			// 3. Click across 6 distinct event cards spaced across the day.
			const hopLatencies: number[] = [];
			const eventCards = timeline.locator('[data-timeline-event]');
			const visibleEventCount = await eventCards.count();
			const hopTargets = Math.min(6, visibleEventCount);
			expect(hopTargets).toBeGreaterThanOrEqual(4);
			const closeDetailButton = page.getByRole('button', {
				name: 'Close event detail',
				exact: true
			});
			for (let index = 0; index < hopTargets; index += 1) {
				if (await closeDetailButton.isVisible()) {
					await closeDetailButton.click();
					await closeDetailButton.waitFor({ state: 'hidden', timeout: 5_000 });
				}
				const cardIndex = Math.floor(
					(index * (visibleEventCount - 1)) / Math.max(1, hopTargets - 1)
				);
				const beforeFrames = await readEventCount(page, 'ReplayFirstFrame');
				const started = performance.now();
				await eventCards.nth(cardIndex).click();
				await waitForEventCountAtLeast(page, 'ReplayFirstFrame', beforeFrames + 1, 10_000);
				hopLatencies.push(performance.now() - started);
			}
			if (await closeDetailButton.isVisible()) {
				await closeDetailButton.click();
				await closeDetailButton.waitFor({ state: 'hidden', timeout: 5_000 });
			}

			// 4. Dense timeline zoom & filter responsiveness on the live server.
			const timelineInteractionLatencies: number[] = [];
			let peakTimelineNodes = 0;
			const zoomInButton = timeline.getByRole('button', { name: 'Zoom timeline in' });
			const measureTimelineAction = async (
				action: () => Promise<void>,
				expectedCheck: () => Promise<void>
			) => {
				const started = performance.now();
				await action();
				await expectedCheck();
				await page.evaluate(
					() =>
						new Promise<void>((resolve) =>
							requestAnimationFrame(() => requestAnimationFrame(() => resolve()))
						)
				);
				timelineInteractionLatencies.push(performance.now() - started);
				const nodeCount = await timeline.evaluate(
					(element) => element.querySelectorAll('*').length
				);
				peakTimelineNodes = Math.max(peakTimelineNodes, nodeCount);
			};

			for (const expectedZoom of ['6h', '1h', '15m']) {
				await measureTimelineAction(
					() => zoomInButton.click(),
					() => expect(timeline).toHaveAttribute('data-timeline-zoom', expectedZoom)
				);
			}
			for (const expectedZoom of ['1h', '6h', '24h']) {
				await measureTimelineAction(
					() => zoomOutButton.click(),
					() => expect(timeline).toHaveAttribute('data-timeline-zoom', expectedZoom)
				);
			}
			const personFilterButton = timeline.getByRole('button', { name: 'Person', exact: true });
			const allFilterButton = timeline.getByRole('button', { name: 'All', exact: true });
			await measureTimelineAction(
				() => personFilterButton.click(),
				() => expect(personFilterButton).toHaveAttribute('aria-pressed', 'true')
			);
			await measureTimelineAction(
				() => allFilterButton.click(),
				() => expect(allFilterButton).toHaveAttribute('aria-pressed', 'true')
			);

			await recordPageLongTasks();

			// 5. Events browser: initial load, search "bunny", thumbnails, zone filter, Open in Keep.
			const eventsStart = performance.now();
			await page.goto(`${baseUrl}/events?date=${manifest.date}`, { waitUntil: 'domcontentloaded' });
			await page
				.locator('[data-event-card]')
				.nth(17)
				.waitFor({ state: 'visible', timeout: 15_000 });
			const eventsInitialPageMs = rounded(performance.now() - eventsStart);

			const searchBox = page.getByRole('searchbox', { name: 'Search events' });
			const searchStart = performance.now();
			await searchBox.fill('bunny');
			await page.waitForFunction(
				() => {
					if (!window.location.search.includes('q=bunny')) return false;
					const cards = [...document.querySelectorAll<HTMLElement>('[data-event-card]')];
					return (
						cards.length === 18 &&
						cards.every((card) => (card.dataset.eventCard ?? '').includes(':bunny-'))
					);
				},
				undefined,
				{ timeout: 10_000 }
			);
			const eventsBunnySearchMs = rounded(performance.now() - searchStart);

			const thumbStart = performance.now();
			const loadedThumbnailCount = await page
				.waitForFunction(
					() => {
						const scroll = document.querySelector<HTMLElement>('[data-event-results-scroll]');
						const scrollRect = scroll?.getBoundingClientRect();
						const cards = [...document.querySelectorAll<HTMLElement>('[data-event-card]')].filter(
							(card) => {
								if (!scrollRect) return true;
								const rect = card.getBoundingClientRect();
								return rect.bottom > scrollRect.top && rect.top < scrollRect.bottom;
							}
						);
						const withPreview = cards.filter(
							(card) => !card.querySelector('[data-event-preview-state="none"]')
						);
						if (withPreview.length === 0) return null;
						const readyImages = withPreview.filter((card) => {
							const img = card.querySelector<HTMLImageElement>('img[data-event-preview-image]');
							return Boolean(img && img.complete && img.naturalWidth > 0);
						});
						return readyImages.length === withPreview.length ? readyImages.length : null;
					},
					undefined,
					{ timeout: 15_000 }
				)
				.then((handle) => handle.jsonValue() as Promise<number>);
			const eventsThumbnailsReadyMs = rounded(performance.now() - thumbStart);

			const zoneInput = page.getByLabel('Zone');
			const zoneStart = performance.now();
			await zoneInput.fill('garden-bed');
			await zoneInput.press('Tab');
			await page.waitForFunction(
				(backyardId) => {
					if (!window.location.search.includes('zone=garden-bed')) return false;
					const cards = [...document.querySelectorAll<HTMLElement>('[data-event-card]')];
					return (
						cards.length > 0 &&
						cards.every((card) => (card.dataset.eventCard ?? '').startsWith(`${backyardId}:bunny-`))
					);
				},
				primaryCamera.id,
				{ timeout: 10_000 }
			);
			const eventsZoneFilterMs = rounded(performance.now() - zoneStart);
			const zoneResultCount = await page.locator('[data-event-card]').count();

			await page.locator('[data-event-card]').first().click();
			const detailDrawer = page.getByRole('complementary', { name: 'Event detail' });
			await detailDrawer.waitFor({ state: 'visible', timeout: 10_000 });
			await expect(detailDrawer).toContainText('garden-bed');
			await expect(detailDrawer).toContainText('bunny-');
			await recordPageLongTasks();

			const openKeepStart = performance.now();
			await detailDrawer.getByRole('link', { name: 'Open at this moment' }).click();
			await page.waitForURL(/\/keep\?/, { timeout: 10_000 });
			await waitForEventCountAtLeast(page, 'ReplayFirstFrame', 1, 15_000);
			const eventsToKeepFrameMs = rounded(performance.now() - openKeepStart);

			// 6. Camera switch & steady-state playback on the 30-second continuous hero segment.
			const cameraSwitcher = page.locator('[data-camera-switcher]');
			await cameraSwitcher.waitFor({ state: 'visible' });
			await cameraSwitcher.getByRole('button', { name: /^Choose camera,/ }).click();
			const framesBeforeSwitch = await readEventCount(page, 'ReplayFirstFrame');
			const switchStart = performance.now();
			await page.locator(`[data-camera-option="${secondaryCamera.id}"]`).click();
			await waitForEventCountAtLeast(page, 'ReplayFirstFrame', framesBeforeSwitch + 1, 15_000);
			const cameraSwitchMs = rounded(performance.now() - switchStart);

			await recordPageLongTasks();

			// Navigate directly to the 30s continuous hero segment to verify smooth steady playback.
			await page.goto(keepUrl, { waitUntil: 'domcontentloaded' });
			await waitForEventCountAtLeast(page, 'ReplayFirstFrame', 1, 15_000);
			await waitForNextDecodedVideoFrame(page, 0, 10_000);
			const sampleMs = 3_000;
			const baselineQuality = await readVideoQuality(page);
			await page.waitForTimeout(sampleMs);
			const finalQuality = await readVideoQuality(page);
			const refills = await readEventCount(page, 'ReplayRefill');
			await recordPageLongTasks();

			const presentedFrames = Math.max(
				0,
				finalQuality.totalVideoFrames - baselineQuality.totalVideoFrames
			);
			const droppedFrames = Math.max(
				0,
				finalQuality.droppedVideoFrames - baselineQuality.droppedVideoFrames
			);
			const presentedFps = rounded(presentedFrames / (sampleMs / 1_000));
			const mediaSecondsAdvanced = rounded(finalQuality.currentTime - baselineQuality.currentTime);
			const maxLongTaskMs = rounded(Math.max(0, ...longTaskSamples));

			const report: PlaybackBenchmarkReport = {
				schemaVersion: 1,
				label: reportLabel,
				generatedAt: new Date().toISOString(),
				environment: {
					platform: platform(),
					release: release(),
					arch: arch(),
					cpus: cpus().length,
					memoryGiB: rounded(totalmem() / 1024 ** 3),
					ci: Boolean(process.env.CI)
				},
				corpus: {
					date: manifest.date,
					cameras: manifest.cameras.length,
					recordingCount: manifest.recordingCount,
					fragmentCount: manifest.fragmentCount,
					eventCount: manifest.eventCount,
					bunnyEventCount: manifest.bunnyEventCount,
					thumbnailCount: manifest.thumbnailCount,
					seedDurationMs: manifest.seedDurationMs
				},
				budgets,
				metrics: {
					keepColdLoad: {
						firstFragmentMs: rounded(coldDetails.firstFragmentMs),
						firstFrameMs: rounded(coldDetails.firstFrameMs),
						timelineEventsReadyMs,
						videoWidth: coldDetails.videoWidth,
						videoHeight: coldDetails.videoHeight
					},
					scrubbing: {
						cold: summarize(coldScrubLatencies),
						revisit: summarize(revisitScrubLatencies),
						all: summarize([...coldScrubLatencies, ...revisitScrubLatencies]),
						memoryCacheHits: scrubRenderStats.memoryCacheHits,
						decoderRenders: scrubRenderStats.decoderRenders,
						commitToFrameMs: scrubCommitToFrameMs
					},
					eventHopping: summarize(hopLatencies),
					timelineInteractions: {
						...summarize(timelineInteractionLatencies),
						peakTimelineNodes
					},
					eventsBrowse: {
						initialPageMs: eventsInitialPageMs,
						bunnySearchMs: eventsBunnySearchMs,
						thumbnailsReadyMs: eventsThumbnailsReadyMs,
						thumbnailCount: loadedThumbnailCount,
						zoneFilterMs: eventsZoneFilterMs,
						zoneResultCount,
						openInKeepFrameMs: eventsToKeepFrameMs
					},
					cameraSwitch: {
						switchToFrameMs: cameraSwitchMs
					},
					steadyPlayback: {
						sampleMs,
						presentedFrames,
						droppedFrames,
						presentedFps,
						mediaSecondsAdvanced,
						refills
					},
					maxLongTaskMs,
					consoleErrors: consoleErrors.length,
					requestFailures: requestFailures.length
				}
			};

			await mkdir(reportDirectory, { recursive: true });
			const jsonPath = path.join(reportDirectory, `${reportLabel}.json`);
			const markdownPath = path.join(reportDirectory, `${reportLabel}.md`);
			const markdown = formatMarkdownReport(report);
			await writeFile(jsonPath, `${JSON.stringify(report, null, 2)}\n`);
			await writeFile(markdownPath, markdown);
			console.log(`\n${markdown}`);
			console.log(`Wrote ${jsonPath}`);

			expect(consoleErrors, `Unexpected console errors: ${consoleErrors.join('; ')}`).toEqual([]);
			expect(requestFailures, `Unexpected request failures: ${requestFailures.join('; ')}`).toEqual(
				[]
			);
			expect(serverStderr, `KeepPeek stderr contained unexpected fatal messages`).not.toContain(
				'panicked'
			);

			if (enforceBudgets) {
				expect(report.metrics.keepColdLoad.firstFrameMs).toBeLessThanOrEqual(
					budgets.keepFirstFrameMs
				);
				expect(report.metrics.keepColdLoad.timelineEventsReadyMs).toBeLessThanOrEqual(
					budgets.keepTimelineEventsReadyMs
				);
				expect(report.metrics.scrubbing.cold.p95Ms).toBeLessThanOrEqual(budgets.scrubColdP95Ms);
				expect(report.metrics.scrubbing.revisit.p95Ms).toBeLessThanOrEqual(
					budgets.scrubRevisitP95Ms
				);
				expect(report.metrics.scrubbing.memoryCacheHits).toBeGreaterThanOrEqual(4);
				expect(report.metrics.scrubbing.commitToFrameMs).toBeLessThanOrEqual(budgets.scrubCommitMs);
				expect(report.metrics.eventHopping.p95Ms).toBeLessThanOrEqual(budgets.eventHopP95Ms);
				expect(report.metrics.timelineInteractions.p95Ms).toBeLessThanOrEqual(
					budgets.timelineInteractionP95Ms
				);
				expect(report.metrics.timelineInteractions.peakTimelineNodes).toBeLessThanOrEqual(
					budgets.maxTimelineNodes
				);
				expect(report.metrics.eventsBrowse.initialPageMs).toBeLessThanOrEqual(
					budgets.eventsInitialPageMs
				);
				expect(report.metrics.eventsBrowse.bunnySearchMs).toBeLessThanOrEqual(
					budgets.eventsBunnySearchMs
				);
				expect(report.metrics.eventsBrowse.thumbnailsReadyMs).toBeLessThanOrEqual(
					budgets.eventsThumbnailsReadyMs
				);
				expect(report.metrics.eventsBrowse.zoneFilterMs).toBeLessThanOrEqual(
					budgets.eventsZoneFilterMs
				);
				expect(report.metrics.eventsBrowse.openInKeepFrameMs).toBeLessThanOrEqual(
					budgets.eventsToKeepFrameMs
				);
				expect(report.metrics.cameraSwitch.switchToFrameMs).toBeLessThanOrEqual(
					budgets.cameraSwitchMs
				);
				expect(report.metrics.steadyPlayback.presentedFps).toBeGreaterThanOrEqual(
					budgets.steadyMinFps
				);
				expect(report.metrics.steadyPlayback.mediaSecondsAdvanced).toBeGreaterThanOrEqual(
					budgets.steadyMinMediaSeconds
				);
			}
		} finally {
			await context.close();
		}
	});
});
