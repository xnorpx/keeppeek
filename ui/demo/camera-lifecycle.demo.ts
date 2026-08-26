import { createHash } from 'node:crypto';
import { execFileSync, spawn } from 'node:child_process';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { basename, join, resolve } from 'node:path';
import { expect, test, type Page } from '@playwright/test';
import {
	assertDemoRecordingCovers,
	assertH264OnlyVideo,
	createFfprobeDurationArgs,
	createFfprobeStreamsArgs,
	createSilentDemoVideoMuxArgs,
	finalizeDemoRecordingDirectory,
	parseFfprobeDurationMs
} from '../src/lib/server/storybook/demo-video';
import { createDemoWebVtt } from '../src/lib/storybook/demo';
import { cameraLifecycleStory } from './camera-lifecycle.story';

type CameraDraft = {
	ip: string;
	displayName: string;
	username: string;
	password: string;
	onvifPort: string;
	httpPort: string;
	mainRtspUrl: string;
	subRtspUrl: string;
	backend: string;
	transport: string;
};

const { demo } = cameraLifecycleStory;
const viewport = demo.viewport;
const outputDirectory = resolve(process.env.DEMO_OUTPUT_DIR ?? 'test-results/demo-videos/assets');
const recordingDirectory = resolve('test-results/demo-playwright/recordings');
const cameraDraftPath = resolve('../target/ui-logging-e2e/camera-draft.json');
const scenarioStem = join(outputDirectory, cameraLifecycleStory.paper.scenarioId);
const recordingTailMs = 500;

test('add a verified camera through the real server', async ({ browser }, testInfo) => {
	const draft = await readCameraDraft();
	await mkdir(outputDirectory, { recursive: true });
	await rm(recordingDirectory, { recursive: true, force: true });
	await mkdir(recordingDirectory, { recursive: true });
	await Promise.all(
		['mp4', 'vtt', 'json', 'webm'].map((extension) =>
			rm(`${scenarioStem}.${extension}`, { force: true })
		)
	);
	const context = await browser.newContext({
		viewport,
		colorScheme: 'dark',
		recordVideo: { dir: recordingDirectory, size: viewport }
	});
	const pageCreatedAt = performance.now();
	const page = await context.newPage();
	const recording = page.video();
	let contextClosed = false;
	let recordingCompleted = false;

	try {
		await page.goto('/cameras');
		await expect(page.getByText('No cameras configured.')).toBeVisible();
		await documentReady(page);
		const demoStartAt = performance.now();

		await waitForAction(page, demoStartAt, 'role=link[name="Add camera"]');
		await page.getByRole('link', { name: 'Add camera', exact: true }).click();
		await expect(page).toHaveURL(/\/cameras\/new$/);
		const wizard = page.locator('[data-desktop-camera-wizard]');
		await wizard.getByLabel('Address or RTSP URL').fill(draft.mainRtspUrl);
		await wizard.getByLabel('Username').fill(draft.username);
		await wizard.getByLabel('Password').fill(draft.password);

		await waitForAction(page, demoStartAt, 'role=button[name="Continue"]', 0);
		await wizard.getByRole('button', { name: 'Continue' }).click();
		await expect(wizard.getByRole('heading', { name: 'Connection options' })).toBeVisible();
		await wizard.getByLabel('Protocol').selectOption(draft.backend);
		await wizard.getByLabel('Transport').selectOption(draft.transport);
		await wizard.getByLabel('ONVIF port').fill(draft.onvifPort);
		await wizard.getByLabel('HTTP port').fill(draft.httpPort);

		await waitForAction(page, demoStartAt, 'role=button[name="Continue"]', 1);
		await wizard.getByRole('button', { name: 'Continue' }).click();
		await wizard.getByLabel(/Recording stream/).fill(draft.mainRtspUrl);
		await wizard.getByLabel(/Live stream/).fill(draft.subRtspUrl);
		await waitForAction(page, demoStartAt, 'role=button[name="Verify streams"]');
		await wizard.getByRole('button', { name: 'Verify streams' }).click();
		await expect(
			wizard.getByText('KeepPeek received authenticated video evidence.', { exact: true })
		).toBeVisible();

		await waitForAction(page, demoStartAt, 'role=button[name="Continue"]', 2);
		await wizard.getByRole('button', { name: 'Continue' }).click();
		await wizard.getByLabel('Camera name').fill(draft.displayName);
		await waitForAction(page, demoStartAt, 'role=button[name="Continue"]', 3);
		await wizard.getByRole('button', { name: 'Continue' }).click();
		await expect(wizard.getByRole('heading', { name: 'Review & save' })).toBeVisible();
		await waitForAction(page, demoStartAt, 'role=button[name="Save camera"]');
		await wizard.getByRole('button', { name: 'Save camera' }).click();
		await expect(page.getByRole('region', { name: 'Camera saved' })).toBeVisible();
		await expect(page.getByRole('link', { name: 'Open camera' })).toHaveAttribute(
			'href',
			`/camera?camera=${encodeURIComponent(draft.ip)}`
		);
		await page.locator(demo.completionSignal.selector).waitFor({
			state: demo.completionSignal.state,
			timeout: 60_000
		});
		const elapsedMs = performance.now() - demoStartAt;
		if (elapsedMs > demo.durationMs) {
			throw new Error(`Camera lifecycle exceeded its ${demo.durationMs}ms story timeline`);
		}
		await waitUntil(page, demoStartAt, demo.durationMs);
		await page.waitForTimeout(recordingTailMs);

		const video = page.video();
		if (!video) throw new Error('Playwright did not create the camera lifecycle recording');
		await context.close();
		contextClosed = true;
		const rawVideoPath = await video.path();
		const recordingPreRollMs = Math.max(0, Math.round(demoStartAt - pageCreatedAt));
		const videoDurationMs = await probeDurationMs(rawVideoPath);
		assertDemoRecordingCovers({
			demoDurationMs: demo.durationMs,
			videoDurationMs,
			recordingPreRollMs
		});

		const mp4Path = `${scenarioStem}.mp4`;
		const captionsPath = `${scenarioStem}.vtt`;
		const metadataPath = `${scenarioStem}.json`;
		await writeFile(captionsPath, createDemoWebVtt(demo));
		await runProcess('ffmpeg', [
			'-loglevel',
			'error',
			...createSilentDemoVideoMuxArgs({
				videoPath: rawVideoPath,
				outputPath: mp4Path,
				durationMs: demo.durationMs,
				recordingPreRollMs
			})
		]);
		const mp4DurationMs = await probeDurationMs(mp4Path);
		if (Math.abs(mp4DurationMs - demo.durationMs) > 100) {
			throw new Error(`H.264 MP4 duration ${mp4DurationMs}ms does not match ${demo.durationMs}ms`);
		}
		assertH264OnlyVideo(await runProcess('ffprobe', createFfprobeStreamsArgs(mp4Path), true));
		const metadata = {
			...cameraLifecycleStory,
			recording: {
				schemaVersion: 1,
				commitSha: commitSha(),
				recordedAt: new Date().toISOString(),
				fixtureSha256: await fixtureHash([
					'demo/camera-lifecycle.story.ts',
					'demo/camera-lifecycle.demo.ts',
					'playwright.demo.config.ts',
					'scripts/start-logging-e2e-server.ts',
					'src/routes/cameras/new/+page.svelte'
				]),
				recordingPreRollMs,
				mp4FileName: basename(mp4Path),
				captionsFileName: basename(captionsPath),
				videoCodec: 'h264',
				streamCount: 1
			}
		};
		await writeFile(metadataPath, `${JSON.stringify(metadata, null, 2)}\n`);
		recordingCompleted = true;
		console.log(
			JSON.stringify({
				scenarioId: cameraLifecycleStory.paper.scenarioId,
				videoDurationMs,
				recordingPreRollMs,
				mp4DurationMs,
				mp4: mp4Path,
				captions: captionsPath,
				metadata: metadataPath
			})
		);
	} finally {
		if (!contextClosed) await context.close().catch(() => {});
		await finalizeDemoRecordingDirectory({ recordingDirectory, completed: recordingCompleted });
		if (!recordingCompleted && recording) {
			const rawVideoPath = await recording.path().catch(() => null);
			if (rawVideoPath) {
				await testInfo.attach('failed-camera-lifecycle-demo.webm', {
					path: rawVideoPath,
					contentType: 'video/webm'
				});
			}
		}
	}
});

async function readCameraDraft(): Promise<CameraDraft> {
	return JSON.parse(await readFile(cameraDraftPath, 'utf8')) as CameraDraft;
}

async function documentReady(page: Page): Promise<void> {
	await page.evaluate(async () => {
		await document.fonts.ready;
		await new Promise<void>((resolveFrame) => requestAnimationFrame(() => resolveFrame()));
		await new Promise<void>((resolveFrame) => requestAnimationFrame(() => resolveFrame()));
	});
}

async function waitForAction(
	page: Page,
	demoStartAt: number,
	selector: string,
	occurrence = 0
): Promise<void> {
	const action = demo.actions.filter(
		(candidate) => 'selector' in candidate && candidate.selector === selector
	)[occurrence];
	if (!action) throw new Error(`Camera lifecycle story has no action for ${selector}`);
	await waitUntil(page, demoStartAt, action.atMs);
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
