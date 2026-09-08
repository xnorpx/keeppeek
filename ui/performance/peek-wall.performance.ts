import { mkdir, writeFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { expect, test, type Page } from '@playwright/test';

type ResourceSample = {
	admitted: number;
	decoders: number;
	decodedFrames: number;
	cpuPercent: number;
	heapMiB: number;
};

async function sampleResources(page: Page): Promise<ResourceSample[]> {
	const devtools = await page.context().newCDPSession(page);
	await devtools.send('Performance.enable');
	let previous = await devtools.send('Performance.getMetrics');
	const samples: ResourceSample[] = [];
	for (let index = 0; index < 12; index += 1) {
		await page.waitForTimeout(500);
		const current = await devtools.send('Performance.getMetrics');
		const value = (metrics: typeof current, name: string) =>
			metrics.metrics.find((metric) => metric.name === name)?.value ?? 0;
		const elapsed = value(current, 'Timestamp') - value(previous, 'Timestamp');
		const cpu = value(current, 'TaskDuration') - value(previous, 'TaskDuration');
		const media = await page.evaluate(() => {
			const videos = [...document.querySelectorAll<HTMLVideoElement>('[data-peek-camera] video')];
			return {
				admitted: document.querySelectorAll('[data-peek-camera] [data-status="live"]').length,
				decoders: videos.filter((video) => video.srcObject !== null).length,
				decodedFrames: videos.reduce(
					(total, video) => total + video.getVideoPlaybackQuality().totalVideoFrames,
					0
				)
			};
		});
		samples.push({
			...media,
			cpuPercent: elapsed > 0 ? (cpu / elapsed) * 100 : 0,
			heapMiB: value(current, 'JSHeapUsedSize') / 1024 ** 2
		});
		previous = current;
	}
	await devtools.detach();
	return samples;
}

function percentile(values: number[], fraction: number): number {
	const ordered = values.toSorted((left, right) => left - right);
	return Number((ordered[Math.ceil(ordered.length * fraction) - 1] ?? 0).toFixed(3));
}

function summarizeResources(samples: ResourceSample[]) {
	return {
		admittedMax: Math.max(0, ...samples.map((sample) => sample.admitted)),
		decodersMax: Math.max(0, ...samples.map((sample) => sample.decoders)),
		cpuPercentP50: percentile(
			samples.map((sample) => sample.cpuPercent),
			0.5
		),
		cpuPercentP95: percentile(
			samples.map((sample) => sample.cpuPercent),
			0.95
		),
		heapMiBP95: percentile(
			samples.map((sample) => sample.heapMiB),
			0.95
		),
		decodedFrameDelta: (samples.at(-1)?.decodedFrames ?? 0) - (samples[0]?.decodedFrames ?? 0)
	};
}

function assertResourceBounds(samples: ResourceSample[]): void {
	for (const sample of samples) {
		expect(sample.admitted).toBeLessThanOrEqual(4);
		expect(sample.decoders).toBeLessThanOrEqual(4);
	}
	const summary = summarizeResources(samples);
	expect(summary.cpuPercentP95).toBeLessThan(50);
	expect(summary.heapMiBP95).toBeLessThan(128);
}

test('bounds a nine-camera wall to four live streams and suspends hidden work', async ({
	page
}) => {
	const phase = process.env.KEEPPEEK_WALL_PERF_PHASE === 'baseline' ? 'baseline' : 'final';
	await page.addInitScript(() => {
		Object.defineProperty(navigator, 'hardwareConcurrency', { configurable: true, value: 8 });
	});
	await page.goto('/');
	await expect(page.locator('[data-peek-camera]')).toHaveCount(9);
	await expect(page.locator('[data-peek-camera] [data-status="live"]')).toHaveCount(4, {
		timeout: 60_000
	});
	await expect(page.locator('[data-peek-wall]')).toHaveAttribute('data-peek-wall-state', 'ready');
	const smart = await sampleResources(page);
	let continuous: ResourceSample[] = [];
	if (phase === 'final') {
		await page.getByRole('button', { name: 'Wall display settings' }).click();
		await page.getByRole('radio', { name: 'Continuous', exact: true }).check();
		await page.keyboard.press('Escape');
		await expect(page.locator('[data-peek-wall]')).toHaveAttribute(
			'data-streaming-mode',
			'continuous'
		);
		continuous = await sampleResources(page);
	}
	await page.evaluate(() => {
		Object.defineProperty(document, 'visibilityState', { configurable: true, value: 'hidden' });
		document.dispatchEvent(new Event('visibilitychange'));
	});
	await expect(page.locator('[data-peek-camera] [data-status="live"]')).toHaveCount(0);
	const hidden = await sampleResources(page);
	const measurements = { smart, continuous, hidden };
	const report = {
		phase,
		workload: 'Nine real-media test cameras, four-stream budget, simulated visibility loss',
		viewport: page.viewportSize(),
		browser: page.context().browser()?.version(),
		platform: `${process.platform}/${process.arch}`,
		samplesPerState: 12,
		intervalMs: 500,
		budget: { admitted: 4, decoders: 4, cpuPercentP95: 50, heapMiBP95: 128 },
		states: Object.fromEntries(
			Object.entries(measurements).map(([name, samples]) => [name, summarizeResources(samples)])
		),
		measurements
	};
	const directory = resolve('../target/peek-performance/wall');
	await mkdir(directory, { recursive: true });
	await writeFile(resolve(directory, `${phase}.json`), `${JSON.stringify(report, null, 2)}\n`);
	console.log(JSON.stringify({ ...report, measurements: undefined }, null, 2));
	for (const samples of Object.values(measurements)) {
		assertResourceBounds(samples);
	}
	expect(report.states.hidden.admittedMax).toBe(0);
	expect(report.states.hidden.decodedFrameDelta).toBe(0);
});
