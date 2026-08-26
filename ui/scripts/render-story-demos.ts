import { createHash } from 'node:crypto';
import { execFileSync, spawn } from 'node:child_process';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { basename, join, resolve } from 'node:path';
import { chromium, type Browser, type Page } from 'playwright';
import { createServer } from 'vite';
import {
	assertDemoRecordingCovers,
	assertH264OnlyVideo,
	createFfprobeDurationArgs,
	createFfprobeStreamsArgs,
	createSilentDemoVideoMuxArgs,
	finalizeDemoRecordingDirectory,
	parseFfprobeDurationMs
} from '../src/lib/server/storybook/demo-video';
import {
	createDemoWebVtt,
	demoAssetId,
	type DemoAction,
	type DemoScenarioDefinition,
	validateStoryScenarioMetadata
} from '../src/lib/storybook/demo';
import { demoScenarios } from '../visual-harness/demo-scenarios';

type DemoRecordingMetadata = DemoScenarioDefinition['metadata'] & {
	recording: {
		schemaVersion: 1;
		commitSha: string;
		recordedAt: string;
		fixtureSha256: string;
		recordingPreRollMs: number;
		mp4FileName: string;
		captionsFileName: string;
		videoCodec: 'h264';
		streamCount: 1;
	};
};

const requestedScenarioIds = process.argv.slice(2);
const selectedScenarios =
	requestedScenarioIds.length === 0
		? [...demoScenarios]
		: requestedScenarioIds.map((requestedId) => {
				const matches = demoScenarios.filter(
					(candidate) =>
						candidate.metadata.paper.scenarioId === requestedId ||
						demoAssetId(candidate.metadata) === requestedId
				);
				if (matches.length === 0) throw new Error(`Unknown demo scenario: ${requestedId}`);
				if (matches.length > 1) throw new Error(`Ambiguous demo scenario: ${requestedId}`);
				return matches[0];
			});

const outputDirectory = resolve(process.env.DEMO_OUTPUT_DIR ?? 'test-results/demo-videos/assets');
const recordingDirectory = resolve('test-results/demo-videos/playwright-recordings');
await rm(outputDirectory, { recursive: true, force: true });
await mkdir(outputDirectory, { recursive: true });
await rm(recordingDirectory, { recursive: true, force: true });
await mkdir(recordingDirectory, { recursive: true });

const server = await createServer({
	configFile: resolve('visual-harness/vite.local.config.ts'),
	logLevel: 'error',
	server: { host: '127.0.0.1', port: 0, strictPort: false }
});
let browser: Browser | null = null;
let renderingCompleted = false;

try {
	await server.listen();
	const baseUrl = server.resolvedUrls?.local[0];
	if (!baseUrl) throw new Error('Demo preview server did not publish a local URL');
	browser = await chromium.launch({ headless: true });
	for (const scenario of selectedScenarios) {
		await renderScenario(browser, baseUrl, scenario);
	}
	renderingCompleted = true;
} finally {
	await browser?.close();
	await server.close();
	await finalizeDemoRecordingDirectory({ recordingDirectory, completed: renderingCompleted });
}

async function renderScenario(
	browserInstance: Browser,
	baseUrl: string,
	scenario: DemoScenarioDefinition
): Promise<void> {
	const issues = validateStoryScenarioMetadata(scenario.metadata);
	if (issues.length > 0) {
		throw new Error(issues.map((issue) => `${issue.path} ${issue.message}`).join('; '));
	}
	const demo = scenario.metadata.demo;
	if (!demo) throw new Error(`Scenario ${scenario.metadata.storyId} has no demo metadata`);

	const scenarioId = scenario.metadata.paper.scenarioId;
	const assetId = demoAssetId(scenario.metadata);
	const stem = join(outputDirectory, assetId);
	const mp4Path = `${stem}.mp4`;
	const captionsPath = `${stem}.vtt`;
	const metadataPath = `${stem}.json`;
	const context = await browserInstance.newContext({
		viewport: demo.viewport,
		colorScheme: 'dark',
		recordVideo: { dir: recordingDirectory, size: demo.viewport }
	});
	const pageCreatedAt = performance.now();
	const page = await context.newPage();
	let signalDemoStart: ((startAt: number) => void) | null = null;
	const demoStarted = new Promise<number>((resolveStart) => {
		signalDemoStart = resolveStart;
	});
	await page.exposeFunction('__keepPeekDemoStart', () => {
		signalDemoStart?.(performance.now());
		signalDemoStart = null;
	});
	const browserErrors: string[] = [];
	page.on('pageerror', (error) => browserErrors.push(error.message));
	page.on('console', (message) => {
		if (message.type() === 'error') browserErrors.push(message.text());
	});

	const previewUrl = new URL('local-preview.html', baseUrl);
	previewUrl.searchParams.set('scenario', scenario.previewScenarioId);
	previewUrl.searchParams.set('demo', 'true');
	previewUrl.searchParams.set('demoAsset', assetId);
	await page.goto(previewUrl.href);
	const demoStartAt = await withTimeout(
		demoStarted,
		15_000,
		`Story ${scenarioId} did not signal demo-start`
	);
	await page.locator('html[data-demo-ready="true"]').waitFor({ state: 'attached' });
	const bounds = await page.locator('[data-paper-scenario]').boundingBox();
	if (
		!bounds ||
		Math.round(bounds.width) !== demo.viewport.width ||
		Math.round(bounds.height) !== demo.viewport.height
	) {
		throw new Error(
			`${scenarioId} demo frame does not fill ${demo.viewport.width}x${demo.viewport.height}`
		);
	}

	for (const action of demo.actions) await performAction(page, action, demoStartAt);
	await waitUntil(page, demoStartAt, demo.durationMs);
	await page.locator(demo.completionSignal.selector).waitFor({
		state: demo.completionSignal.state,
		timeout: 5_000
	});
	await page.waitForTimeout(750);
	if (browserErrors.length > 0) {
		throw new Error(`Demo ${scenarioId} emitted browser errors:\n${browserErrors.join('\n')}`);
	}

	const video = page.video();
	if (!video) throw new Error(`Playwright did not create a video for ${scenarioId}`);
	await context.close();
	const recordedPath = await video.path();
	const videoDurationMs = await probeDurationMs(recordedPath);
	const recordingPreRollMs = Math.max(0, Math.round(demoStartAt - pageCreatedAt));
	assertDemoRecordingCovers({
		demoDurationMs: demo.durationMs,
		videoDurationMs,
		recordingPreRollMs
	});

	await writeFile(captionsPath, createDemoWebVtt(demo));
	await runProcess(
		'ffmpeg',
		createSilentDemoVideoMuxArgs({
			videoPath: recordedPath,
			outputPath: mp4Path,
			durationMs: demo.durationMs,
			recordingPreRollMs
		})
	);
	const mp4DurationMs = await probeDurationMs(mp4Path);
	if (Math.abs(mp4DurationMs - demo.durationMs) > 100) {
		throw new Error(`Rendered MP4 duration ${mp4DurationMs}ms does not match ${demo.durationMs}ms`);
	}
	assertH264OnlyVideo(await runProcess('ffprobe', createFfprobeStreamsArgs(mp4Path), true));

	const metadata: DemoRecordingMetadata = {
		...scenario.metadata,
		recording: {
			schemaVersion: 1,
			commitSha: commitSha(),
			recordedAt: new Date().toISOString(),
			fixtureSha256: await fixtureHash(scenario.fixtureSources),
			recordingPreRollMs,
			mp4FileName: basename(mp4Path),
			captionsFileName: basename(captionsPath),
			videoCodec: 'h264',
			streamCount: 1
		}
	};
	await writeFile(metadataPath, `${JSON.stringify(metadata, null, 2)}\n`);
	console.log(
		JSON.stringify({
			scenarioId,
			assetId,
			videoDurationMs,
			recordingPreRollMs,
			mp4DurationMs,
			mp4: mp4Path,
			captions: captionsPath,
			metadata: metadataPath
		})
	);
}

async function performAction(page: Page, action: DemoAction, demoStartAt: number): Promise<void> {
	await waitUntil(page, demoStartAt, action.atMs);
	if (action.kind === 'click') {
		await page.locator(action.selector).click();
		return;
	}
	if (action.kind === 'press') {
		if (action.selector) await page.locator(action.selector).press(action.key);
		else await page.keyboard.press(action.key);
		return;
	}

	const target = page.locator(action.selector);
	await target.waitFor({ state: 'visible' });
	const bounds = await target.boundingBox();
	if (!bounds) throw new Error(`Unable to resolve drag target: ${action.selector}`);
	const startX = bounds.x + bounds.width / 2;
	const startY = bounds.y + bounds.height / 2;
	const steps = action.steps ?? 20;
	await page.mouse.move(startX, startY);
	await page.mouse.down();
	for (let step = 1; step <= steps; step += 1) {
		await page.mouse.move(
			startX + (action.deltaX * step) / steps,
			startY + (action.deltaY * step) / steps
		);
		await page.waitForTimeout(action.durationMs / steps);
	}
	if (action.holdAfterMs) await page.waitForTimeout(action.holdAfterMs);
	await page.mouse.up();
}

async function waitUntil(page: Page, demoStartAt: number, targetMs: number): Promise<void> {
	const remainingMs = targetMs - (performance.now() - demoStartAt);
	if (remainingMs > 0) await page.waitForTimeout(remainingMs);
}

async function probeDurationMs(mediaPath: string): Promise<number> {
	const output = await runProcess('ffprobe', createFfprobeDurationArgs(mediaPath), true);
	return parseFfprobeDurationMs(output);
}

async function runProcess(command: string, args: string[], captureStdout = false): Promise<string> {
	return new Promise((resolveProcess, rejectProcess) => {
		const processHandle = spawn(command, args, {
			stdio: captureStdout ? ['ignore', 'pipe', 'pipe'] : 'inherit'
		});
		let stdout = '';
		let stderr = '';
		if (captureStdout) {
			processHandle.stdout?.setEncoding('utf8');
			processHandle.stderr?.setEncoding('utf8');
			processHandle.stdout?.on('data', (chunk: string) => (stdout += chunk));
			processHandle.stderr?.on('data', (chunk: string) => (stderr += chunk));
		}
		processHandle.on('error', rejectProcess);
		processHandle.on('close', (exitCode) => {
			if (exitCode !== 0) {
				rejectProcess(new Error(`${command} failed: ${stderr.trim()}`));
				return;
			}
			resolveProcess(stdout);
		});
	});
}

async function fixtureHash(sources: readonly string[]): Promise<string> {
	const hash = createHash('sha256');
	for (const source of [...sources].toSorted()) {
		hash.update(source);
		hash.update('\0');
		hash.update(await readFile(resolve(source)));
		hash.update('\0');
	}
	return hash.digest('hex');
}

function commitSha(): string {
	if (process.env.GITHUB_SHA?.trim()) return process.env.GITHUB_SHA.trim();
	try {
		return execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
	} catch {
		return 'local';
	}
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
	let timeout: ReturnType<typeof setTimeout> | undefined;
	try {
		return await Promise.race([
			promise,
			new Promise<T>((_, reject) => {
				timeout = setTimeout(() => reject(new Error(message)), timeoutMs);
			})
		]);
	} finally {
		if (timeout) clearTimeout(timeout);
	}
}
