import { expect, test, type Page } from '@playwright/test';
import { cpus, platform, release } from 'node:os';

type GestureSample = {
	inputMs: number[];
	frameMs: number[];
	loadEvents: number;
	emptyEvents: number;
	layoutShift: number;
	framesBefore: number;
	framesAfter: number;
	mediaConnected: boolean;
	sourceUnchanged: boolean;
	zoom: number;
};

function percentile(values: readonly number[], fraction: number): number {
	const sorted = values.toSorted((left, right) => left - right);
	return sorted[Math.max(0, Math.ceil(sorted.length * fraction) - 1)] ?? 0;
}

async function prepareLive(page: Page, baseUrl: string) {
	const created = page.waitForResponse(
		(response) =>
			new URL(response.url()).pathname === '/create' && response.request().method() === 'POST'
	);
	await page.goto(new URL('/viewer?camera=127.0.0.1', baseUrl).href);
	expect((await created).status()).toBe(201);
	const live = page.locator('[data-peek-focus-stage] [data-camera-id="127.0.0.1"]');
	await expect(live).toHaveAttribute('data-status', 'live', { timeout: 30_000 });
	await expect(live).not.toHaveAttribute('data-pending-stream', { timeout: 30_000 });
	await expect
		.poll(() =>
			live
				.locator('video')
				.evaluate((media: HTMLVideoElement) => media.getVideoPlaybackQuality().totalVideoFrames)
		)
		.toBeGreaterThan(10);
	return live;
}

async function sampleGestures(page: Page): Promise<GestureSample> {
	return page.evaluate(collectGestureSample, {
		inputMs: [],
		frameMs: [],
		loadEvents: 0,
		emptyEvents: 0,
		layoutShift: 0,
		framesBefore: 0,
		framesAfter: 0,
		mediaConnected: true,
		sourceUnchanged: true,
		zoom: 1
	});
}

async function collectGestureSample(sample: GestureSample): Promise<GestureSample> {
	const live = document.querySelector<HTMLElement>('[data-peek-focus-stage] [data-camera-id]')!;
	const media = live.querySelector('video')!;
	const viewport = live.querySelector<HTMLElement>('[role="application"]') ?? live;
	const rect = viewport.getBoundingClientRect();
	const source = media.srcObject;
	sample.framesBefore = media.getVideoPlaybackQuality().totalVideoFrames;
	const abort = new AbortController();
	media.addEventListener('loadstart', () => (sample.loadEvents += 1), { signal: abort.signal });
	media.addEventListener('emptied', () => (sample.emptyEvents += 1), { signal: abort.signal });
	const observer = new PerformanceObserver((list) => {
		for (const entry of list.getEntries()) {
			sample.layoutShift += (entry as PerformanceEntry & { value: number }).value;
		}
	});
	observer.observe({ type: 'layout-shift' });
	const centerX = rect.left + rect.width / 2;
	const centerY = rect.top + rect.height / 2;
	const pointer = (type: string, clientX: number, clientY: number) =>
		viewport.dispatchEvent(
			new PointerEvent(type, {
				pointerId: 1,
				pointerType: 'mouse',
				clientX,
				clientY,
				bubbles: true,
				cancelable: true
			})
		);
	viewport.dispatchEvent(new KeyboardEvent('keydown', { key: '0' }));
	viewport.dispatchEvent(
		new MouseEvent('dblclick', { clientX: centerX, clientY: centerY, cancelable: true })
	);
	await new Promise(requestAnimationFrame);
	let previousFrame = await new Promise<number>(requestAnimationFrame);
	pointer('pointerdown', centerX, centerY);
	for (let frame = 0; frame < 120; frame += 1) {
		const now = await new Promise<number>(requestAnimationFrame);
		sample.frameMs.push(now - previousFrame);
		previousFrame = now;
		const started = performance.now();
		for (let movement = 0; movement < 20; movement += 1) {
			const angle = ((frame * 20 + movement) / 120) * Math.PI;
			pointer('pointermove', centerX + Math.sin(angle) * 40, centerY + Math.cos(angle) * 30);
		}
		viewport.dispatchEvent(
			new WheelEvent('wheel', {
				altKey: true,
				deltaY: frame % 2 === 0 ? -1 : 1,
				clientX: centerX,
				clientY: centerY,
				cancelable: true
			})
		);
		sample.inputMs.push(performance.now() - started);
	}
	pointer('pointerup', centerX, centerY);
	await new Promise(requestAnimationFrame);
	sample.framesAfter = media.getVideoPlaybackQuality().totalVideoFrames;
	sample.mediaConnected = media.isConnected;
	sample.sourceUnchanged = media.srcObject === source;
	sample.zoom = Number(
		live.querySelector('[data-focused-media]')?.getAttribute('data-digital-zoom') ?? 1
	);
	observer.disconnect();
	abort.abort();
	return sample;
}

async function measureLive(page: Page, baseUrl: string) {
	let creates = 0;
	let deletes = 0;
	const browserErrors: string[] = [];
	page.on('request', (request) => {
		if (request.method() !== 'POST') return;
		if (new URL(request.url()).pathname === '/create') creates += 1;
		if (new URL(request.url()).pathname === '/delete') deletes += 1;
	});
	page.on('pageerror', (error) => browserErrors.push(error.message));
	const live = await prepareLive(page, baseUrl);
	const sessionId = await live.getAttribute('data-session-id');
	const initialCreates = creates;
	const initialDeletes = deletes;
	const runs: GestureSample[] = [];
	for (let run = 0; run < 3; run += 1) runs.push(await sampleGestures(page));
	await expect(live).toHaveAttribute('data-session-id', sessionId!);
	expect(creates - initialCreates).toBe(0);
	expect(deletes - initialDeletes).toBe(0);
	expect(browserErrors).toEqual([]);
	for (const sample of runs) {
		expect(sample.loadEvents).toBe(0);
		expect(sample.emptyEvents).toBe(0);
		expect(sample.layoutShift).toBe(0);
		expect(sample.framesAfter).toBeGreaterThan(sample.framesBefore);
		expect(sample.mediaConnected).toBe(true);
		expect(sample.sourceUnchanged).toBe(true);
	}
	const input = runs.flatMap((sample) => sample.inputMs);
	const frames = runs.flatMap((sample) => sample.frameMs);
	return {
		inputP50Ms: percentile(input, 0.5),
		inputP95Ms: percentile(input, 0.95),
		frameP50Ms: percentile(frames, 0.5),
		frameP95Ms: percentile(frames, 0.95),
		sampleCount: input.length,
		additionalSessions: creates - initialCreates,
		runs
	};
}

test('real focused video stays stable under a bounded gesture workload', async ({
	browser,
	page,
	baseURL
}, testInfo) => {
	test.setTimeout(90_000);
	await page.setViewportSize({ width: 1440, height: 900 });
	const baseline = process.env.KEEPPEEK_ZOOM_BASELINE === '1';
	const result = await measureLive(page, baseURL!);
	expect(result.inputP95Ms).toBeLessThan(8);
	for (const sample of result.runs) expect(sample.zoom).toBeCloseTo(baseline ? 1 : 2, 1);
	const evidence = {
		environment: {
			os: `${platform()} ${release()}`,
			cpu: cpus()[0]?.model,
			browser: browser.version(),
			viewport: '1440x900',
			fixture: 'H.264 640x360 at 15fps'
		},
		workload: '3 runs x 120 frames, 20 pointer moves + 1 Alt-wheel event per frame',
		variant: baseline ? 'original-ui' : 'digital-zoom',
		budget: { inputP95Ms: 8 },
		result
	};
	await testInfo.attach('digital-zoom-performance.json', {
		body: JSON.stringify(evidence, null, 2),
		contentType: 'application/json'
	});
	console.log(
		'DIGITAL_ZOOM_PERFORMANCE',
		JSON.stringify({
			environment: evidence.environment,
			variant: evidence.variant,
			result: {
				inputP50Ms: result.inputP50Ms,
				inputP95Ms: result.inputP95Ms,
				frameP50Ms: result.frameP50Ms,
				frameP95Ms: result.frameP95Ms
			}
		})
	);
	await page.screenshot({ path: testInfo.outputPath('digital-zoom-real-live.png') });
});
