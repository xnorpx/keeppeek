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
	parseFfprobeDurationMs
} from '../src/lib/server/storybook/demo-video';
import { createDemoWebVtt } from '../src/lib/storybook/demo';
import { nineCameraLiveStory } from './nine-camera-live.story';

const cameraIds = Array.from({ length: 9 }, (_, index) => `192.0.2.${101 + index}`);
const backendMetricsUrl = 'http://127.0.0.1:4318/metrics';
const { demo } = nineCameraLiveStory;
const viewport = demo.viewport;
const outputDirectory = resolve(process.env.DEMO_OUTPUT_DIR ?? 'test-results/demo-videos/assets');
const recordingDirectory = resolve('test-results/nine-camera-demo-playwright/recordings');
const draftsPath = resolve('../target/nine-camera-demo/camera-drafts.json');
const scenarioStem = join(outputDirectory, nineCameraLiveStory.paper.scenarioId);
const recordingTailMs = 500;
const nineCameraHardwareConcurrency = 18;

type CameraDraft = {
	id: string;
	name: string;
	startAtSeconds: number;
	ip: string;
	displayName: string;
	username: string;
	password: string;
	mainRtspUrl: string;
	subRtspUrl: string;
	backend: string;
	transport: string;
};

type CameraDrafts = {
	schemaVersion: 1;
	fixtureSha256: string;
	selection: {
		safeBeforeSeconds: number;
		safeAfterSeconds: number;
		excludedBlackIntervals: Array<{ startSeconds: number; endSeconds: number }>;
	};
	cameras: CameraDraft[];
};

test('adds nine randomized RTSP cameras in Settings and shows the production WebRTC wall', async ({
	browser
}) => {
	const cameraDrafts = JSON.parse(await readFile(draftsPath, 'utf8')) as CameraDrafts;
	expect(cameraDrafts.cameras.map((camera) => camera.id)).toEqual(cameraIds);
	expect(new Set(cameraDrafts.cameras.map((camera) => camera.startAtSeconds)).size).toBe(
		cameraIds.length
	);
	const firstSave = demo.actions.find(
		(action) => action.selector === 'role=button[name="Save camera"]'
	);
	if (!firstSave) throw new Error('Nine-camera story has no Save camera action');
	expect(cameraDrafts.selection.safeAfterSeconds).toBeGreaterThanOrEqual(
		(demo.durationMs - firstSave.atMs) / 1_000
	);
	for (const camera of cameraDrafts.cameras) {
		expect(
			cameraDrafts.selection.excludedBlackIntervals.some(
				(interval) =>
					camera.startAtSeconds - cameraDrafts.selection.safeBeforeSeconds < interval.endSeconds &&
					camera.startAtSeconds + cameraDrafts.selection.safeAfterSeconds > interval.startSeconds
			)
		).toBe(false);
	}

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
	await page.addInitScript((hardwareConcurrency) => {
		Object.defineProperty(navigator, 'hardwareConcurrency', { value: hardwareConcurrency });
	}, nineCameraHardwareConcurrency);
	let contextClosed = false;

	try {
		const browserErrors: string[] = [];
		page.on('console', (message) => {
			if (message.type() === 'error') browserErrors.push(message.text());
		});
		page.on('pageerror', (error) => browserErrors.push(error.message));
		const createResponse = page.waitForResponse(
			(response) => response.url().endsWith('/create') && response.request().method() === 'POST',
			{ timeout: 30_000 }
		);
		await page.goto('/');
		const response = await createResponse;
		expect(response.status(), await response.text()).toBe(201);
		await expect(page.locator('[data-peek-camera]')).toHaveCount(0);
		await documentReady(page);
		const demoStartAt = performance.now();

		await waitForAction(page, demoStartAt, 'a[aria-label="Settings"]');
		await page.getByRole('link', { name: 'Settings', exact: true }).click();
		await showCameraSetup(page);
		await expect(page.getByText('No cameras configured.')).toBeVisible();

		for (const [index, draft] of cameraDrafts.cameras.entries()) {
			await waitForAction(page, demoStartAt, 'role=button[name="Add camera"]', index);
			await page.getByRole('button', { name: 'Add camera', exact: true }).click();
			const form = cameraEditor(page);
			await fillCameraDraft(page, form, draft);
			await waitForAction(page, demoStartAt, 'role=button[name="Save camera"]', index);
			await form.getByRole('button', { name: 'Save camera' }).click();
			await expect(page.getByText('Camera settings saved.')).toBeVisible();
			await expect(page.getByText(`${index + 1} configured`, { exact: true })).toBeVisible();
		}

		await waitForAction(page, demoStartAt, 'role=button[name="Restart"]');
		const restarted = page.waitForEvent('load', { timeout: 30_000 });
		await page.getByRole('button', { name: 'Restart', exact: true }).click();
		await restarted;
		await showCameraSetup(page);
		await expect(page.getByText('9 configured', { exact: true })).toBeVisible();
		await waitForCameraIngress(page);

		await waitForNineLiveCameras(page, async () => {
			await waitForAction(page, demoStartAt, 'a[aria-label="Peek"]');
			await page.getByRole('link', { name: 'Peek', exact: true }).click();
		});

		const diagnosticsSelector = '[data-camera-id="192.0.2.101"] button[data-peek-camera-label]';
		for (const index of [0, 1]) {
			await waitForAction(page, demoStartAt, diagnosticsSelector, index);
			await page.locator(diagnosticsSelector).click();
			const diagnostics = page.locator('[data-web-rtc-diagnostics="192.0.2.101"]');
			if (index === 0) await expect(diagnostics).toBeVisible();
			else await expect(diagnostics).toBeHidden();
		}
		await page.locator(demo.completionSignal.selector).waitFor({
			state: demo.completionSignal.state,
			timeout: 60_000
		});
		await waitUntil(page, demoStartAt, demo.durationMs);
		await page.waitForTimeout(recordingTailMs);
		expect(browserErrors).toEqual([]);

		const video = page.video();
		if (!video) throw new Error('Playwright did not create the nine-camera recording');
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
			...nineCameraLiveStory,
			recording: {
				schemaVersion: 1,
				commitSha: commitSha(),
				recordedAt: new Date().toISOString(),
				fixtureSha256: await fixtureHash([
					'demo/nine-camera-live.story.ts',
					'demo/nine-camera-live.demo.ts',
					'playwright.nine-camera-demo.config.ts',
					'scripts/prepare-nine-camera-demo-fixture.ts',
					'scripts/start-nine-camera-demo-server.ts',
					'../crates/retina/src/testutil/fake_camera.rs',
					'../crates/test-camera/src/lib.rs'
				]),
				sourceMediaSha256: cameraDrafts.fixtureSha256,
				cameras: cameraDrafts.cameras.map(({ id, name, startAtSeconds }) => ({
					id,
					name,
					startAtSeconds
				})),
				recordingPreRollMs,
				mp4FileName: basename(mp4Path),
				captionsFileName: basename(captionsPath),
				videoCodec: 'h264',
				streamCount: 1,
				cameraCount: cameraIds.length
			}
		};
		await writeFile(metadataPath, `${JSON.stringify(metadata, null, 2)}\n`);
		console.log(
			JSON.stringify({
				scenarioId: nineCameraLiveStory.paper.scenarioId,
				cameraCount: cameraIds.length,
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
		await rm(recordingDirectory, { recursive: true, force: true });
	}
});

async function waitForCameraIngress(page: Page): Promise<void> {
	await expect
		.poll(
			async () => {
				try {
					const response = await page.request.get(backendMetricsUrl);
					if (!response.ok()) return [];
					const metrics = await response.text();
					return cameraIds.filter(
						(cameraId) =>
							metricValue(metrics, 'keeppeek_camera_online', { camera_id: cameraId }) === 1 &&
							(metricValue(metrics, 'keeppeek_camera_ingress_frames_per_second', {
								camera_id: cameraId,
								stream: 'video_sub'
							}) ?? 0) > 0
					);
				} catch {
					return [];
				}
			},
			{ timeout: 90_000 }
		)
		.toEqual(cameraIds);
}

function metricValue(
	metrics: string,
	metricName: string,
	labels: Readonly<Record<string, string>>
): number | null {
	const line = metrics
		.split('\n')
		.find(
			(candidate) =>
				candidate.startsWith(`${metricName}{`) &&
				Object.entries(labels).every(([label, value]) => candidate.includes(`${label}="${value}"`))
		);
	if (!line) return null;
	const value = Number(line.slice(line.lastIndexOf(' ') + 1));
	return Number.isFinite(value) ? value : null;
}

async function waitForNineLiveCameras(page: Page, navigate: () => Promise<unknown>): Promise<void> {
	await navigate();
	await expect(page.locator('[data-peek-camera]')).toHaveCount(cameraIds.length);

	for (const cameraId of cameraIds) {
		const liveView = page.locator(`[data-camera-id="${cameraId}"]`);
		const tile = page.locator(`[data-peek-camera="${cameraId}"]`);
		await expect(liveView).toHaveAttribute('data-status', 'live', { timeout: 90_000 });
		await expect(liveView).toHaveAttribute('data-frame-activity', 'active', { timeout: 90_000 });
		const video = liveView.locator('video');
		await expect(video).toBeVisible();
		await expect
			.poll(
				async () =>
					video.evaluate((element) => {
						const videoElement = element as HTMLVideoElement;
						return `${videoElement.videoWidth}x${videoElement.videoHeight}:${videoElement.getVideoPlaybackQuality().totalVideoFrames}`;
					}),
				{ timeout: 90_000 }
			)
			.toMatch(/^640x360:[1-9]\d*$/);
		await expect
			.poll(
				async () =>
					video.evaluate((element) => {
						const videoElement = element as HTMLVideoElement;
						const canvas = document.createElement('canvas');
						canvas.width = 32;
						canvas.height = 18;
						const context = canvas.getContext('2d', { willReadFrequently: true });
						if (!context) return 0;
						context.drawImage(videoElement, 0, 0, canvas.width, canvas.height);
						const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
						let total = 0;
						for (let index = 0; index < pixels.length; index += 4) {
							total += pixels[index] + pixels[index + 1] + pixels[index + 2];
						}
						return total / (canvas.width * canvas.height * 3);
					}),
				{ timeout: 90_000 }
			)
			.toBeGreaterThan(8);
		await expect(tile).toHaveAttribute('data-peek-camera-state', /^(?:live|degraded)$/);
		await expect(tile).not.toContainText('Reconnecting');
		await expect(tile).not.toContainText('NO SIGNAL');
	}
}

async function showCameraSetup(page: Page): Promise<void> {
	const title = page.getByText('Camera setup', { exact: true });
	await expect(title).toBeVisible();
	await title.scrollIntoViewIfNeeded();
}

function cameraEditor(page: Page) {
	return page
		.locator('form')
		.filter({ has: page.getByRole('heading', { name: 'Add camera', exact: true }) });
}

async function fillCameraDraft(
	page: Page,
	form: ReturnType<typeof cameraEditor>,
	draft: CameraDraft
): Promise<void> {
	const fields = [
		['IP address', draft.ip],
		['Display name', draft.displayName],
		['Username', draft.username],
		['Password', draft.password],
		['Main RTSP stream URL', draft.mainRtspUrl],
		['Sub RTSP stream URL', draft.subRtspUrl]
	] as const;
	for (const [label, value] of fields) {
		await form.getByLabel(label, { exact: true }).fill(value);
		await page.waitForTimeout(120);
	}
	await form.getByLabel('Backend').selectOption(draft.backend);
	await form.getByLabel('Transport').selectOption(draft.transport);
}

async function documentReady(page: Page): Promise<void> {
	await page.evaluate(async () => {
		await document.fonts.ready;
		await new Promise<void>((resolveFrame) => requestAnimationFrame(() => resolveFrame()));
		await new Promise<void>((resolveFrame) => requestAnimationFrame(() => resolveFrame()));
	});
}

async function waitUntil(page: Page, demoStartAt: number, targetMs: number): Promise<void> {
	const remainingMs = targetMs - (performance.now() - demoStartAt);
	if (remainingMs > 0) await page.waitForTimeout(remainingMs);
}

async function waitForAction(
	page: Page,
	demoStartAt: number,
	selector: string,
	occurrence = 0
): Promise<void> {
	const action = demo.actions.filter((candidate) => candidate.selector === selector)[occurrence];
	if (!action) throw new Error(`Nine-camera story has no action for ${selector}`);
	await waitUntil(page, demoStartAt, action.atMs);
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
