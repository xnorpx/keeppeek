import { createHash } from 'node:crypto';
import { execFileSync, spawn } from 'node:child_process';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { basename, join, resolve } from 'node:path';
import { expect, test, type Browser, type Page } from '@playwright/test';
import {
	assertDemoRecordingCovers,
	assertH264OnlyVideo,
	createFfprobeDurationArgs,
	createFfprobeStreamsArgs,
	createSilentDemoVideoMuxArgs,
	parseFfprobeDurationMs
} from '../src/lib/server/storybook/demo-video';
import { createDemoWebVtt } from '../src/lib/storybook/demo';
import { cameraLifecycleStory } from './camera-lifecycle.story';

type CameraDraft = {
	ip: string;
	displayName: string;
	manufacturer: string;
	onvifPort: string;
	httpPort: string;
	mainRtspUrl: string;
	subRtspUrl: string;
	backend: string;
	transport: string;
};

const cameraId = '127.0.0.1';
const { demo } = cameraLifecycleStory;
const viewport = demo.viewport;
const outputDirectory = resolve(process.env.DEMO_OUTPUT_DIR ?? 'test-results/demo-videos/assets');
const recordingDirectory = resolve('test-results/demo-playwright/recordings');
const scenarioStem = join(outputDirectory, cameraLifecycleStory.paper.scenarioId);

test('delete and re-add the same camera through the real server', async ({ browser }) => {
	const draft = await readStableCameraDraft(browser);
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
	let contextClosed = false;

	try {
		await waitForStableH264Camera(page);
		await documentReady(page);
		const demoStartAt = performance.now();

		await waitForAction(page, demoStartAt, 'a[aria-label="Settings"]');
		await page.getByRole('link', { name: 'Settings' }).click();
		await expect(page).toHaveURL(/\/settings$/);
		await showCameraSetup(page);

		await waitForAction(page, demoStartAt, 'role=button[name="Remove"]');
		page.once('dialog', (dialog) => dialog.accept());
		await page.getByRole('button', { name: 'Remove', exact: true }).click();
		await expect(page.getByText('No cameras configured.')).toBeVisible();
		await expect(
			page.getByText('Camera removed. Apply changes to update the server.')
		).toBeVisible();

		await waitForAction(page, demoStartAt, 'role=button[name="Add camera"]');
		await page.getByRole('button', { name: 'Add camera', exact: true }).click();
		const form = cameraEditor(page, 'Add camera');
		await fillCameraDraft(page, form, draft);
		await waitForAction(page, demoStartAt, 'role=button[name="Save camera"]');
		await form.getByRole('button', { name: 'Save camera' }).click();
		await expect(page.getByText('Camera settings saved.')).toBeVisible();
		await expect(page.getByText('1 configured', { exact: true })).toBeVisible();
		await expect(page.getByRole('button', { name: 'Apply changes' })).toBeVisible();
		await showCameraSetup(page);
		await page.locator(demo.completionSignal.selector).waitFor({
			state: demo.completionSignal.state,
			timeout: 60_000
		});
		const elapsedMs = performance.now() - demoStartAt;
		if (elapsedMs > demo.durationMs) {
			throw new Error(`Camera lifecycle exceeded its ${demo.durationMs}ms story timeline`);
		}
		await waitUntil(page, demoStartAt, demo.durationMs);

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
					'scripts/start-logging-e2e-server.ts',
					'src/routes/settings/+page.svelte'
				]),
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
		await rm(recordingDirectory, { recursive: true, force: true });
	}
});

async function readStableCameraDraft(browser: Browser): Promise<CameraDraft> {
	const context = await browser.newContext({ viewport, colorScheme: 'dark' });
	const page = await context.newPage();
	try {
		await waitForStableH264Camera(page);
		await page.getByRole('link', { name: 'Settings' }).click();
		await showCameraSetup(page);
		await page.getByRole('button', { name: 'Edit', exact: true }).click();
		const form = cameraEditor(page, 'Edit camera');
		const draft = {
			ip: await form.getByLabel('IP address').inputValue(),
			displayName: await form.getByLabel('Display name').inputValue(),
			manufacturer: await form.getByLabel('Manufacturer override').inputValue(),
			onvifPort: await form.getByLabel('ONVIF port').inputValue(),
			httpPort: await form.getByLabel('HTTP port').inputValue(),
			mainRtspUrl: await form.getByLabel('Main RTSP stream URL').inputValue(),
			subRtspUrl: await form.getByLabel('Sub RTSP stream URL').inputValue(),
			backend: await form.getByLabel('Backend').inputValue(),
			transport: await form.getByLabel('Transport').inputValue()
		};
		await form.getByRole('button', { name: 'Cancel' }).first().click();
		return draft;
	} finally {
		await context.close();
	}
}

async function waitForStableH264Camera(page: Page, navigate = true): Promise<void> {
	if (navigate) await page.goto('/');
	const liveView = page.locator(`[data-camera-id="${cameraId}"]`);
	await expect(liveView).toHaveAttribute('data-status', 'live', { timeout: 60_000 });
	await expect(liveView).toHaveAttribute('data-codec', /h264/i, { timeout: 60_000 });
	await expect(liveView).toHaveAttribute('data-frame-activity', 'active', { timeout: 60_000 });
	await expect(page.locator(`[data-peek-camera="${cameraId}"]`)).toHaveAttribute(
		'data-peek-camera-state',
		/^(?:live|degraded)$/
	);
}

async function showCameraSetup(page: Page): Promise<void> {
	const title = page.getByText('Camera setup', { exact: true });
	await expect(title).toBeVisible();
	await title.scrollIntoViewIfNeeded();
}

function cameraEditor(page: Page, title: 'Add camera' | 'Edit camera') {
	return page
		.locator('form')
		.filter({ has: page.getByRole('heading', { name: title, exact: true }) });
}

async function fillCameraDraft(
	page: Page,
	form: ReturnType<typeof cameraEditor>,
	draft: CameraDraft
): Promise<void> {
	const fields = [
		['IP address', draft.ip],
		['Display name', draft.displayName],
		['Username', 'test'],
		['Password', 'test'],
		['Manufacturer override', draft.manufacturer],
		['ONVIF port', draft.onvifPort],
		['HTTP port', draft.httpPort],
		['Main RTSP stream URL', draft.mainRtspUrl],
		['Sub RTSP stream URL', draft.subRtspUrl]
	] as const;
	for (const [label, value] of fields) {
		await form.getByLabel(label, { exact: true }).fill(value);
		await page.waitForTimeout(150);
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
