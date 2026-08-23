import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { basename, resolve } from 'node:path';
import { chromium, type Browser, type Page } from 'playwright';
import { createServer } from 'vite';
import { mockControlPeer } from '../e2e/fixtures/control-peer';
import { diagnosisVisualHealth } from '../e2e/fixtures/diagnosis';

type CaptureConfig = {
	width: number;
	height: number;
	theme: 'dark' | 'light';
	candidatePath: string;
	referencePath: string;
	route?: string;
};

type ComparisonMetrics = {
	width: number;
	height: number;
	pixels: number;
	exactMismatchPixels: number;
	thresholdMismatchPixels: number;
	threshold: number;
	thresholdMismatchRatio: number;
	mismatchCurve: Record<string, { pixels: number; ratio: number }>;
	meanAbsoluteChannelDifference: number;
	maximumChannelDifference: number;
};

type FrameDiagnostics = {
	fonts: Record<string, boolean>;
	elements: Record<
		string,
		{
			text: string;
			x: number;
			y: number;
			width: number;
			height: number;
			fontFamily: string;
			fontSize: string;
			fontWeight: string;
			lineHeight: string;
			letterSpacing: string;
		}
	>;
};

const captures: Record<string, CaptureConfig> = {
	'keep.desktop.timeline-anatomy': {
		width: 1280,
		height: 720,
		theme: 'dark',
		candidatePath: 'test-results/board-04-keep-timeline-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/04-keep-timeline-anatomy.png'
	},
	'peek.desktop.live-wall': {
		width: 1440,
		height: 860,
		theme: 'dark',
		candidatePath: 'test-results/board-06-peek-live-wall-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/06-peek-live-wall.png'
	},
	'camera.desktop.details-ptz': {
		width: 1440,
		height: 2059,
		theme: 'dark',
		candidatePath: 'test-results/board-07-camera-details-ptz-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/07-camera-details-ptz.png'
	},
	'peek.desktop.layout-editor': {
		width: 1440,
		height: 840,
		theme: 'dark',
		candidatePath: 'test-results/board-08-layout-editor-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/08-layout-editor.png'
	},
	'peek.desktop.layout-registry': {
		width: 1440,
		height: 396,
		theme: 'dark',
		candidatePath: 'test-results/board-08-layout-registry-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/08-layout-registry-dialogs.png'
	},
	'keep.desktop.stories': {
		width: 467,
		height: 413,
		theme: 'dark',
		candidatePath: 'test-results/board-09-keep-stories-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/09-keep-stories.png'
	},
	'keep.desktop.calendar': {
		width: 467,
		height: 413,
		theme: 'dark',
		candidatePath: 'test-results/board-09-keep-calendar-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/09-keep-calendar.png'
	},
	'keep.desktop.export-gated': {
		width: 467,
		height: 413,
		theme: 'dark',
		candidatePath: 'test-results/board-09-keep-export-gated-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/09-keep-export-gated.png'
	},
	'keep.desktop.swimlanes': {
		width: 1440,
		height: 363,
		theme: 'dark',
		candidatePath: 'test-results/board-09-keep-swimlanes-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/09-keep-swimlanes.png'
	},
	'events.desktop.browse': {
		width: 1440,
		height: 820,
		theme: 'dark',
		candidatePath: 'test-results/board-10-events-browse-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/10-events-browse.png'
	},
	'events.desktop.detail': {
		width: 1440,
		height: 669,
		theme: 'dark',
		candidatePath: 'test-results/board-10-event-detail-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/10-event-detail.png'
	},
	'cameras.desktop.fleet': {
		width: 1440,
		height: 624,
		theme: 'dark',
		candidatePath: 'test-results/board-11-camera-fleet-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/11-camera-fleet.png'
	},
	'cameras.desktop.add-wizard': {
		width: 1440,
		height: 663,
		theme: 'dark',
		candidatePath: 'test-results/board-12-add-camera-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/12-add-camera-stream-evidence.png'
	},
	'settings.desktop.storage-retention': {
		width: 1440,
		height: 1163,
		theme: 'dark',
		candidatePath: 'test-results/board-13-storage-retention-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/13-storage-retention.png'
	},
	'settings.desktop.event-sources': {
		width: 1440,
		height: 1048,
		theme: 'dark',
		candidatePath: 'test-results/board-14-event-sources-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/14-event-sources.png'
	},
	'health.desktop.overview': {
		width: 1440,
		height: 1302,
		theme: 'dark',
		candidatePath: 'test-results/board-15-health-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/15-health-server-client.png'
	},
	'settings.desktop.access': {
		width: 1440,
		height: 1249,
		theme: 'dark',
		candidatePath: 'test-results/board-16-access-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/16-access-roles.png'
	},
	'settings.desktop.integrations': {
		width: 1440,
		height: 869,
		theme: 'dark',
		candidatePath: 'test-results/board-17-integrations-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/17-integrations.png'
	},
	'settings.desktop.notifications': {
		width: 1440,
		height: 1075,
		theme: 'dark',
		candidatePath: 'test-results/board-18-notifications-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/18-notifications.png'
	},
	'groups.desktop.administration': {
		width: 1440,
		height: 416,
		theme: 'dark',
		candidatePath: 'test-results/board-19-groups-administration-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/19-groups-administration.png'
	},
	'groups.desktop.participant': {
		width: 1440,
		height: 420,
		theme: 'dark',
		candidatePath: 'test-results/board-19-groups-participant-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/19-groups-participant.png'
	},
	'settings.desktop.appearance-system-logs': {
		width: 1440,
		height: 581,
		theme: 'dark',
		candidatePath: 'test-results/board-20-appearance-system-logs-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/20-appearance-system-logs.png'
	},
	'setup.desktop.first-run': {
		width: 1440,
		height: 785,
		theme: 'dark',
		candidatePath: 'test-results/board-21-first-run-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/21-first-run-empty-states.png'
	},
	'peek.mobile.live': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-22-mobile-peek-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/22-mobile-peek.png'
	},
	'keep.mobile.timeline': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-22-mobile-keep-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/22-mobile-keep.png'
	},
	'events.mobile.browse': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-22-mobile-events-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/22-mobile-events.png'
	},
	'settings.desktop.camera-defaults': {
		width: 1374,
		height: 806,
		theme: 'dark',
		candidatePath: 'test-results/board-23-camera-defaults-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/23-camera-defaults-content.png'
	},
	'camera.mobile.details-ptz': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-24-mobile-camera-live-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/24-mobile-camera-live.png'
	},
	'camera.mobile.ptz': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-24-mobile-camera-ptz-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/24-mobile-camera-ptz.png'
	},
	'camera.mobile.settings': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-24-mobile-camera-settings-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/24-mobile-camera-settings.png'
	},
	'cameras.mobile.add-wizard': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-25-mobile-find-connect-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/25-mobile-find-connect.png'
	},
	'cameras.mobile.add-streams': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-25-mobile-streams-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/25-mobile-streams.png'
	},
	'cameras.mobile.add-review': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-25-mobile-review-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/25-mobile-review.png'
	},
	'health.mobile.overview': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-26-mobile-health-overview-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/26-mobile-health-overview.png'
	},
	'health.mobile.camera-issue': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-26-mobile-health-issue-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/26-mobile-health-issue.png'
	},
	'health.mobile.stream-evidence': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-26-mobile-stream-evidence-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/26-mobile-stream-evidence.png'
	},
	'settings.mobile.administration': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-27-mobile-administration-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/27-mobile-administration-index.png'
	},
	'settings.mobile.camera-defaults': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-27-mobile-camera-defaults-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/27-mobile-camera-defaults.png'
	},
	'settings.mobile.access': {
		width: 390,
		height: 844,
		theme: 'dark',
		candidatePath: 'test-results/board-27-mobile-access-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/27-mobile-access.png'
	},
	'health.desktop.camera-diagnosis': {
		width: 1440,
		height: 776,
		theme: 'dark',
		candidatePath: 'test-results/board-30-camera-diagnosis-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/30-camera-diagnosis.png'
	},
	'peek.desktop.focus-history': {
		width: 464,
		height: 262,
		theme: 'dark',
		candidatePath: 'test-results/board-31-focus-history-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/31-focus-history.png'
	},
	'peek.desktop.history-keep': {
		width: 464,
		height: 262,
		theme: 'dark',
		candidatePath: 'test-results/board-31-history-keep-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/31-history-keep.png'
	},
	'peek.waiting.first-keyframe': {
		width: 462,
		height: 172,
		theme: 'dark',
		candidatePath: 'test-results/board-33-first-keyframe-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/33-first-keyframe.png'
	},
	'keep.waiting.cold-seek': {
		width: 462,
		height: 172,
		theme: 'dark',
		candidatePath: 'test-results/board-33-cold-seek-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/33-cold-seek.png'
	},
	'cameras.waiting.discovery': {
		width: 462,
		height: 172,
		theme: 'dark',
		candidatePath: 'test-results/board-33-discovery-progress-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/33-discovery-progress.png'
	},
	'events.empty.no-results': {
		width: 462,
		height: 238,
		theme: 'dark',
		candidatePath: 'test-results/board-33-no-results-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/33-no-results.png'
	},
	'settings.waiting.applying': {
		width: 462,
		height: 238,
		theme: 'dark',
		candidatePath: 'test-results/board-33-settings-applying-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/33-settings-applying.png'
	},
	'cameras.waiting.fleet-skeleton': {
		width: 462,
		height: 238,
		theme: 'dark',
		candidatePath: 'test-results/board-33-fleet-skeleton-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/33-fleet-skeleton.png'
	},
	'keep.desktop.export-lifecycle': {
		width: 1440,
		height: 369,
		theme: 'dark',
		candidatePath: 'test-results/board-29-export-lifecycle-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/29-export-job-lifecycle-states.png'
	},
	'peek.desktop.light-theme': {
		width: 1440,
		height: 362,
		theme: 'light',
		candidatePath: 'test-results/board-34-light-theme-candidate.png',
		referencePath: 'design/paper/keeppeek-nvr-v34/references/34-light-theme-peek.png'
	}
};

const scenarioId = process.argv[2];
const capture = scenarioId === undefined ? undefined : captures[scenarioId];
if (!scenarioId || !capture) {
	throw new Error(`Usage: bun scripts/capture-paper-story.ts <${Object.keys(captures).join('|')}>`);
}

const candidatePath = resolve(capture.candidatePath);
const artifactPrefix = candidatePath.replace(/-candidate\.png$/, '');
const overlayPath = `${artifactPrefix}-overlay.png`;
const differencePath = `${artifactPrefix}-difference.png`;
const metricsPath = `${artifactPrefix}-metrics.json`;
await mkdir(resolve('test-results'), { recursive: true });

const server = await createServer({
	configFile: resolve(capture.route ? 'vite.config.ts' : 'visual-harness/vite.local.config.ts'),
	logLevel: 'error',
	server: { host: '127.0.0.1', port: 0, strictPort: false }
});
let browser: Browser | null = null;

try {
	await server.listen();
	const url = server.resolvedUrls?.local[0];
	if (!url) throw new Error('Visual preview server did not publish a local URL');
	browser = await chromium.launch({ headless: true });
	const page = await browser.newPage({
		viewport: { width: capture.width, height: capture.height },
		colorScheme: capture.theme
	});
	let frame;
	if (capture.route) {
		await page.addInitScript(
			(theme) => localStorage.setItem('keeppeek-theme', theme),
			capture.theme
		);
		await mockControlPeer(page, {
			health: diagnosisVisualHealth,
			capabilityIds: ['keeppeek.runtime-config.v1']
		});
		await page.goto(new URL(capture.route, url).href);
		frame = page.locator('[data-keyboard-ready]');
		await page.locator('[data-keyboard-ready="true"]').waitFor();
		await page.getByRole('heading', { name: 'Back Yard', exact: true }).waitFor();
	} else {
		const previewUrl = new URL('local-preview.html', url);
		previewUrl.searchParams.set('scenario', scenarioId);
		await page.goto(previewUrl.href);
		frame = page.locator(`[data-paper-scenario="${scenarioId}"]`);
	}
	await frame.waitFor({ state: 'visible' });
	await page.evaluate(() => document.fonts.ready);
	const bounds = await frame.boundingBox();
	if (
		!bounds ||
		Math.round(bounds.width) !== capture.width ||
		Math.round(bounds.height) !== capture.height
	) {
		throw new Error(
			`${scenarioId} rendered ${bounds?.width ?? 0}x${bounds?.height ?? 0}; expected ${capture.width}x${capture.height}`
		);
	}
	await frame.screenshot({ path: candidatePath, animations: 'disabled' });
	const diagnostics = await collectFrameDiagnostics(page);

	const metrics = await compareImages(
		browser,
		capture,
		await readFile(resolve(capture.referencePath)),
		await readFile(candidatePath),
		overlayPath,
		differencePath
	);
	await writeFile(
		metricsPath,
		`${JSON.stringify({ scenarioId, ...metrics, diagnostics }, null, 2)}\n`
	);
	console.log(
		JSON.stringify({
			scenarioId,
			candidate: basename(candidatePath),
			overlay: basename(overlayPath),
			difference: basename(differencePath),
			metrics
		})
	);
} finally {
	await browser?.close();
	await server.close();
}

async function collectFrameDiagnostics(page: Page): Promise<FrameDiagnostics> {
	return page.evaluate(() => {
		const selectors = [
			'h1',
			'[data-cold-seek] > p:nth-child(1)',
			'[data-cold-seek] > p:nth-child(2)',
			'[data-cold-seek] > p:nth-child(3)',
			'[data-peek-camera="front-door"] [data-peek-camera-region="compact-status"] > div:first-child span:first-child',
			'[data-peek-camera="front-door"] [data-peek-camera-region="compact-status"] > div:first-child span:last-child',
			'[data-peek-camera="front-door"] [data-peek-camera-region="compact-status"] > div:last-child p',
			'[data-peek-camera="front-door"] [data-peek-camera-region="compact-status"] > div:last-child > span',
			'[data-peek-camera="porch"] [data-peek-camera-region="compact-status"] > div:first-child span:first-child',
			'[data-peek-camera="porch"] [data-peek-camera-region="compact-status"] > div:first-child span:last-child',
			'[data-peek-camera="porch"] [data-peek-camera-region="compact-status"] > div:last-child p:first-child',
			'[data-peek-camera="porch"] [data-peek-camera-region="compact-status"] > div:last-child p:last-child',
			'[data-peek-camera="porch"] [data-peek-camera-region="compact-status"] > div:last-child > span',
			'[data-peek-camera="back-yard"] [data-peek-camera-region="compact-status"] > div:first-child span:first-child',
			'[data-peek-camera="back-yard"] [data-peek-camera-region="compact-status"] > div:nth-child(2) p:first-child',
			'[data-peek-camera="back-yard"] [data-peek-camera-region="compact-status"] > div:nth-child(2) p:nth-child(2)',
			'[data-peek-camera="back-yard"] [data-peek-camera-region="compact-status"] > div:nth-child(2) span',
			'[data-peek-camera="back-yard"] [data-peek-camera-region="compact-status"] > div:last-child p',
			'[data-peek-camera="back-yard"] [data-peek-camera-region="compact-status"] > div:last-child > span',
			'footer > span:first-child',
			'footer > span:nth-child(2)',
			'footer > span:nth-child(3)',
			'footer > span:last-child'
		];
		const elements = Object.fromEntries(
			selectors.flatMap((selector) => {
				const element = document.querySelector<HTMLElement>(selector);
				if (!element) return [];
				const bounds = element.getBoundingClientRect();
				const style = getComputedStyle(element);
				return [
					[
						selector,
						{
							text: element.textContent?.trim().replace(/\s+/g, ' ') ?? '',
							x: bounds.x,
							y: bounds.y,
							width: bounds.width,
							height: bounds.height,
							fontFamily: style.fontFamily,
							fontSize: style.fontSize,
							fontWeight: style.fontWeight,
							lineHeight: style.lineHeight,
							letterSpacing: style.letterSpacing
						}
					] as const
				];
			})
		);
		return {
			fonts: {
				'Archivo 400': document.fonts.check('400 16px Archivo'),
				'Archivo 600': document.fonts.check('600 16px Archivo'),
				'Archivo 700': document.fonts.check('700 16px Archivo'),
				'IBM Plex Mono 400': document.fonts.check('400 16px "IBM Plex Mono"')
			},
			elements
		};
	});
}

async function compareImages(
	browser: Browser,
	capture: CaptureConfig,
	reference: Buffer,
	candidate: Buffer,
	overlayPath: string,
	differencePath: string
): Promise<ComparisonMetrics> {
	const page = await browser.newPage({
		viewport: { width: capture.width, height: capture.height }
	});
	try {
		await page.setContent(`
			<style>html, body { margin: 0; } canvas { display: block; }</style>
			<canvas id="overlay" width="${capture.width}" height="${capture.height}"></canvas>
			<canvas id="difference" width="${capture.width}" height="${capture.height}"></canvas>
		`);
		const metrics = await page.evaluate(
			async ({ width, height, referenceDataUrl, candidateDataUrl }) => {
				function loadImage(source: string): Promise<HTMLImageElement> {
					return new Promise((resolveImage, rejectImage) => {
						const image = new Image();
						image.onload = () => resolveImage(image);
						image.onerror = () => rejectImage(new Error('Unable to decode comparison image'));
						image.src = source;
					});
				}

				const [referenceImage, candidateImage] = await Promise.all([
					loadImage(referenceDataUrl),
					loadImage(candidateDataUrl)
				]);
				if (
					referenceImage.naturalWidth !== width ||
					referenceImage.naturalHeight !== height ||
					candidateImage.naturalWidth !== width ||
					candidateImage.naturalHeight !== height
				) {
					throw new Error('Paper reference and candidate dimensions must match the authored frame');
				}

				const scratch = document.createElement('canvas');
				scratch.width = width;
				scratch.height = height;
				const scratchContext = scratch.getContext('2d', { willReadFrequently: true });
				const overlayContext = (
					document.querySelector<HTMLCanvasElement>('#overlay') as HTMLCanvasElement
				).getContext('2d');
				const differenceContext = (
					document.querySelector<HTMLCanvasElement>('#difference') as HTMLCanvasElement
				).getContext('2d');
				if (!scratchContext || !overlayContext || !differenceContext) {
					throw new Error('Canvas comparison context is unavailable');
				}

				scratchContext.drawImage(referenceImage, 0, 0);
				const referencePixels = scratchContext.getImageData(0, 0, width, height);
				scratchContext.clearRect(0, 0, width, height);
				scratchContext.drawImage(candidateImage, 0, 0);
				const candidatePixels = scratchContext.getImageData(0, 0, width, height);
				const overlayPixels = overlayContext.createImageData(width, height);
				const differencePixels = differenceContext.createImageData(width, height);
				const threshold = 16;
				const curveThresholds = [16, 24, 32, 48, 64, 96, 128] as const;
				const curveCounts = Object.fromEntries(
					curveThresholds.map((curveThreshold) => [String(curveThreshold), 0])
				) as Record<string, number>;
				let exactMismatchPixels = 0;
				let thresholdMismatchPixels = 0;
				let absoluteDifference = 0;
				let maximumChannelDifference = 0;

				for (let offset = 0; offset < referencePixels.data.length; offset += 4) {
					let pixelMaximum = 0;
					for (let channel = 0; channel < 3; channel += 1) {
						const referenceChannel = referencePixels.data[offset + channel];
						const candidateChannel = candidatePixels.data[offset + channel];
						const difference = Math.abs(referenceChannel - candidateChannel);
						overlayPixels.data[offset + channel] = Math.round(
							(referenceChannel + candidateChannel) / 2
						);
						absoluteDifference += difference;
						pixelMaximum = Math.max(pixelMaximum, difference);
						maximumChannelDifference = Math.max(maximumChannelDifference, difference);
					}
					overlayPixels.data[offset + 3] = 255;
					if (pixelMaximum > 0) exactMismatchPixels += 1;
					if (pixelMaximum > threshold) thresholdMismatchPixels += 1;
					for (const curveThreshold of curveThresholds) {
						if (pixelMaximum > curveThreshold) curveCounts[String(curveThreshold)] += 1;
					}
					const unchanged = pixelMaximum <= threshold;
					const referenceLuminance = Math.round(
						(referencePixels.data[offset] +
							referencePixels.data[offset + 1] +
							referencePixels.data[offset + 2]) /
							3
					);
					differencePixels.data[offset] = unchanged ? referenceLuminance * 0.18 : 255;
					differencePixels.data[offset + 1] = unchanged ? referenceLuminance * 0.18 : 0;
					differencePixels.data[offset + 2] = unchanged ? referenceLuminance * 0.18 : 96;
					differencePixels.data[offset + 3] = 255;
				}

				overlayContext.putImageData(overlayPixels, 0, 0);
				differenceContext.putImageData(differencePixels, 0, 0);
				const pixels = width * height;
				const mismatchCurve = Object.fromEntries(
					Object.entries(curveCounts).map(([curveThreshold, count]) => [
						curveThreshold,
						{ pixels: count, ratio: count / pixels }
					])
				);
				return {
					width,
					height,
					pixels,
					exactMismatchPixels,
					thresholdMismatchPixels,
					threshold,
					thresholdMismatchRatio: thresholdMismatchPixels / pixels,
					mismatchCurve,
					meanAbsoluteChannelDifference: absoluteDifference / (pixels * 3),
					maximumChannelDifference
				};
			},
			{
				width: capture.width,
				height: capture.height,
				referenceDataUrl: `data:image/png;base64,${reference.toString('base64')}`,
				candidateDataUrl: `data:image/png;base64,${candidate.toString('base64')}`
			}
		);
		await page.locator('#overlay').screenshot({ path: overlayPath });
		await page.locator('#difference').screenshot({ path: differencePath });
		return metrics;
	} finally {
		await page.close();
	}
}
