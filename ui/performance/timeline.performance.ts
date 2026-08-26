import { mkdir, writeFile } from 'node:fs/promises';
import { arch, cpus, platform, release, totalmem } from 'node:os';
import { fileURLToPath } from 'node:url';
import { expect, test, type Page } from '@playwright/test';

type Orientation = 'horizontal' | 'vertical';
type Operation = 'cold_seek_feedback' | 'drag' | 'filter' | 'pan' | 'zoom';
type Sample = {
	durationMs: number;
	nodeCount: number;
	viewportChangeDelta: number;
};
type Summary = {
	samples: number;
	p50Ms: number;
	p95Ms: number;
	maxMs: number;
};

function positiveNumberEnvironment(name: string, fallback: number): number {
	const value = Number(process.env[name] ?? fallback);
	if (!Number.isFinite(value) || value <= 0) throw new Error(`${name} must be positive`);
	return value;
}

function positiveIntegerEnvironment(name: string, fallback: number): number {
	const value = positiveNumberEnvironment(name, fallback);
	if (!Number.isInteger(value)) throw new Error(`${name} must be an integer`);
	return value;
}

const budgetP95Ms = positiveNumberEnvironment('KEEPPEEK_TIMELINE_PERF_BUDGET_MS', 150);
const interactionSamples = positiveIntegerEnvironment('KEEPPEEK_TIMELINE_PERF_SAMPLES', 20);
const initialRenderSamples = positiveIntegerEnvironment(
	'KEEPPEEK_TIMELINE_PERF_INITIAL_SAMPLES',
	10
);
const enforceBudget = process.env.KEEPPEEK_TIMELINE_PERF_ENFORCE !== '0';
const maxTimelineNodes = 1_600;
const reportLabel = (process.env.KEEPPEEK_TIMELINE_PERF_LABEL ?? 'latest').replaceAll(
	/[^a-z0-9_-]/gi,
	'-'
);
const reportDirectory = fileURLToPath(
	new URL('../../target/keep-performance/timeline/', import.meta.url)
);
const fixtures = [
	{
		name: 'desktop',
		orientation: 'vertical' as const,
		viewport: { width: 1440, height: 900 },
		operations: ['zoom', 'pan', 'filter', 'cold_seek_feedback', 'drag'] as const
	},
	{
		name: 'mobile',
		orientation: 'horizontal' as const,
		viewport: { width: 390, height: 844 },
		operations: ['zoom', 'pan', 'cold_seek_feedback', 'drag'] as const
	}
];

function percentile(values: readonly number[], percentileValue: number): number {
	if (values.length === 0) return 0;
	const sorted = values.toSorted((left, right) => left - right);
	return sorted[Math.max(0, Math.ceil(sorted.length * percentileValue) - 1)] ?? 0;
}

function rounded(value: number): number {
	return Number(value.toFixed(2));
}

function summarize(values: readonly number[]): Summary {
	return {
		samples: values.length,
		p50Ms: rounded(percentile(values, 0.5)),
		p95Ms: rounded(percentile(values, 0.95)),
		maxMs: rounded(Math.max(0, ...values))
	};
}

async function installLongTaskCollector(page: Page): Promise<void> {
	await page.addInitScript(() => {
		const benchmarkWindow = window as Window & { __timelineLongTasks?: number[] };
		benchmarkWindow.__timelineLongTasks = [];
		try {
			new PerformanceObserver((entries) => {
				benchmarkWindow.__timelineLongTasks?.push(
					...entries.getEntries().map((entry) => entry.duration)
				);
			}).observe({ entryTypes: ['longtask'] });
		} catch {
			benchmarkWindow.__timelineLongTasks = [];
		}
	});
}

async function loadHarness(page: Page, orientation: Orientation): Promise<Sample> {
	await page.goto(
		`/local-preview.html?scenario=keep.performance.timeline&orientation=${orientation}`,
		{ waitUntil: 'domcontentloaded' }
	);
	await page.locator('[data-timeline-performance-harness]').waitFor();
	await page.waitForFunction(
		() => Number(document.documentElement.dataset.timelineInitialRenderMs) > 0
	);
	return page.evaluate(() => {
		const timeline = document.querySelector<HTMLElement>('[aria-label="Recording timeline"]');
		if (!timeline) throw new Error('Timeline did not mount');
		return {
			durationMs: Number(document.documentElement.dataset.timelineInitialRenderMs),
			nodeCount: timeline.querySelectorAll('*').length,
			viewportChangeDelta: 0
		};
	});
}

async function measureInteraction(
	page: Page,
	orientation: Orientation,
	operation: Operation,
	iteration: number
): Promise<Sample> {
	return page.evaluate(
		async ({ orientation, operation, iteration }) => {
			const root = document.querySelector<HTMLElement>('[data-timeline-performance-harness]');
			const timeline = root?.querySelector<HTMLElement>('[aria-label="Recording timeline"]');
			if (!root || !timeline) throw new Error('Timeline performance harness is unavailable');
			const frame = () =>
				new Promise<void>((resolveFrame) => requestAnimationFrame(() => resolveFrame()));
			const settle = async () => {
				await frame();
				await frame();
			};
			const waitUntil = async (predicate: () => boolean, label: string) => {
				for (let attempt = 0; attempt < 30; attempt += 1) {
					if (predicate()) return;
					await frame();
				}
				throw new Error(`${label} did not produce feedback`);
			};
			const scroller = timeline.querySelector<HTMLElement>(
				orientation === 'vertical'
					? '[aria-label="Recording timeline scroll viewport"]'
					: '[aria-label="Recording timeline scrubber"]'
			);
			if (!scroller) throw new Error('Timeline scroller is unavailable');
			const viewportChangesBefore = Number(root.dataset.viewportChangeCount ?? 0);
			const startedAtMs = performance.now();

			if (operation === 'zoom') {
				const previousZoom = timeline.dataset.timelineZoom;
				const direction = iteration % 2 === 0 ? 'in' : 'out';
				const control = timeline.querySelector<HTMLButtonElement>(
					`button[title="Zoom timeline ${direction}"]`
				);
				if (!control || control.disabled) throw new Error(`Zoom ${direction} is unavailable`);
				control.click();
				await waitUntil(() => timeline.dataset.timelineZoom !== previousZoom, 'Zoom');
			} else if (operation === 'pan') {
				const vertical = orientation === 'vertical';
				const maximum = vertical
					? scroller.scrollHeight - scroller.clientHeight
					: scroller.scrollWidth - scroller.clientWidth;
				const target = Math.max(0, maximum * (iteration % 2 === 0 ? 0.65 : 0.2));
				if (vertical) scroller.scrollTop = target;
				else scroller.scrollLeft = target;
				scroller.dispatchEvent(new Event('scroll', { bubbles: true }));
				await waitUntil(
					() => Math.abs((vertical ? scroller.scrollTop : scroller.scrollLeft) - target) < 1,
					'Pan'
				);
			} else if (operation === 'filter') {
				const label = iteration % 2 === 0 ? 'Motion' : 'All';
				const control = [...timeline.querySelectorAll<HTMLButtonElement>('button')].find(
					(button) => button.textContent?.trim() === label
				);
				if (!control) throw new Error(`${label} filter is unavailable`);
				control.click();
				await waitUntil(() => control.getAttribute('aria-pressed') === 'true', 'Filter');
			} else if (operation === 'drag') {
				const target =
					orientation === 'vertical'
						? timeline.querySelector<HTMLButtonElement>('button[aria-label^="Playback position"]')
						: scroller;
				if (!target) throw new Error('Timeline drag target is unavailable');
				Object.defineProperties(target, {
					setPointerCapture: { configurable: true, value: () => undefined },
					hasPointerCapture: { configurable: true, value: () => true },
					releasePointerCapture: { configurable: true, value: () => undefined }
				});
				const before =
					orientation === 'vertical'
						? target.getAttribute('aria-label')
						: scroller.getAttribute('aria-valuenow');
				const bounds = scroller.getBoundingClientRect();
				const startX = bounds.left + bounds.width / 2;
				const startY = bounds.top + bounds.height / 2;
				const delta = (iteration % 2 === 0 ? 1 : -1) * 48;
				const pointerId = iteration + 10;
				target.dispatchEvent(
					new PointerEvent('pointerdown', {
						bubbles: true,
						button: 0,
						cancelable: true,
						clientX: startX,
						clientY: startY,
						pointerId
					})
				);
				for (let step = 1; step <= 24; step += 1) {
					target.dispatchEvent(
						new PointerEvent('pointermove', {
							bubbles: true,
							button: 0,
							cancelable: true,
							clientX: startX + (orientation === 'horizontal' ? (delta * step) / 24 : 0),
							clientY: startY + (orientation === 'vertical' ? (delta * step) / 24 : 0),
							pointerId
						})
					);
				}
				target.dispatchEvent(
					new PointerEvent('pointerup', {
						bubbles: true,
						button: 0,
						cancelable: true,
						clientX: startX + (orientation === 'horizontal' ? delta : 0),
						clientY: startY + (orientation === 'vertical' ? delta : 0),
						pointerId
					})
				);
				await waitUntil(
					() =>
						(orientation === 'vertical'
							? timeline
									.querySelector('button[aria-label^="Playback position"]')
									?.getAttribute('aria-label')
							: scroller.getAttribute('aria-valuenow')) !== before,
					'Drag'
				);
			} else {
				const seekCountBefore = Number(root.dataset.seekCount ?? 0);
				const bounds = scroller.getBoundingClientRect();
				const fraction = iteration % 2 === 0 ? 0.3 : 0.7;
				const target =
					orientation === 'vertical'
						? timeline.querySelector<HTMLElement>('button[aria-label^="Scroll recording timeline"]')
						: timeline.querySelector<HTMLElement>('[role="presentation"]');
				if (!target) throw new Error('Timeline seek target is unavailable');
				target.dispatchEvent(
					new MouseEvent('click', {
						bubbles: true,
						button: 0,
						clientX: bounds.left + bounds.width * fraction,
						clientY: bounds.top + bounds.height * fraction
					})
				);
				await waitUntil(() => Number(root.dataset.seekCount ?? 0) > seekCountBefore, 'Cold seek');
			}

			await settle();
			return {
				durationMs: performance.now() - startedAtMs,
				nodeCount: timeline.querySelectorAll('*').length,
				viewportChangeDelta: Number(root.dataset.viewportChangeCount ?? 0) - viewportChangesBefore
			};
		},
		{ orientation, operation, iteration }
	);
}

function markdownReport(report: {
	generatedAt: string;
	budgetP95Ms: number;
	maxTimelineNodes: number;
	dataset: { segments: number; events: number; durationHours: number };
	environment: Record<string, string | number>;
	viewports: Array<{
		name: string;
		width: number;
		height: number;
		metrics: Record<string, Summary>;
		peakTimelineNodes: number;
		maxViewportChangesPerDrag: number;
		longTasks: Summary;
	}>;
}): string {
	const lines = [
		'# Keep timeline performance',
		'',
		`- Generated: \`${report.generatedAt}\``,
		`- Dataset: ${report.dataset.durationHours} hours, ${report.dataset.segments} segments, ${report.dataset.events} events`,
		`- Budget: p95 interaction feedback at or below ${report.budgetP95Ms} ms`,
		`- DOM guard: at most ${report.maxTimelineNodes} timeline descendants`,
		`- Browser: \`${report.environment.browser}\``,
		`- Hardware: \`${report.environment.cpu}\`, ${report.environment.logicalCpuCount} logical CPUs, ${report.environment.memoryGiB} GiB RAM`,
		`- OS: \`${report.environment.os}\``,
		'',
		'| Viewport | Metric | Samples | p50 ms | p95 ms | max ms | Result |',
		'| --- | --- | ---: | ---: | ---: | ---: | --- |'
	];
	for (const viewport of report.viewports) {
		for (const [metric, summary] of Object.entries(viewport.metrics)) {
			lines.push(
				`| ${viewport.name} ${viewport.width}x${viewport.height} | ${metric} | ${summary.samples} | ${summary.p50Ms} | ${summary.p95Ms} | ${summary.maxMs} | ${summary.p95Ms <= report.budgetP95Ms ? 'pass' : 'fail'} |`
			);
		}
	}
	lines.push(
		'',
		'| Viewport | Peak timeline nodes | Max viewport callbacks per drag | Long-task p95 ms |'
	);
	lines.push('| --- | ---: | ---: | ---: |');
	for (const viewport of report.viewports) {
		lines.push(
			`| ${viewport.name} ${viewport.width}x${viewport.height} | ${viewport.peakTimelineNodes} | ${viewport.maxViewportChangesPerDrag} | ${viewport.longTasks.p95Ms} |`
		);
	}
	lines.push('', 'Run with `bun run perf:timeline` from `ui/`.');
	return `${lines.join('\n')}\n`;
}

test('keeps dense timeline interaction feedback within budget', async ({ page }) => {
	test.setTimeout(180_000);
	await installLongTaskCollector(page);
	const browser = page.context().browser();
	const cpu = cpus()[0];
	const viewportReports = [];

	for (const fixture of fixtures) {
		await page.setViewportSize(fixture.viewport);
		const initialSamples: Sample[] = [];
		const longTasks: number[] = [];
		for (let iteration = 0; iteration < initialRenderSamples; iteration += 1) {
			initialSamples.push(await loadHarness(page, fixture.orientation));
			longTasks.push(
				...(await page.evaluate(
					() => (window as Window & { __timelineLongTasks?: number[] }).__timelineLongTasks ?? []
				))
			);
		}

		const metricSamples: Record<string, Sample[]> = { initial_render: initialSamples };
		for (const operation of fixture.operations) {
			const samples: Sample[] = [];
			for (let iteration = 0; iteration < interactionSamples; iteration += 1) {
				samples.push(await measureInteraction(page, fixture.orientation, operation, iteration));
			}
			metricSamples[operation] = samples;
		}
		longTasks.push(
			...(await page.evaluate(
				() => (window as Window & { __timelineLongTasks?: number[] }).__timelineLongTasks ?? []
			))
		);
		const allSamples = Object.values(metricSamples).flat();
		viewportReports.push({
			name: fixture.name,
			width: fixture.viewport.width,
			height: fixture.viewport.height,
			metrics: Object.fromEntries(
				Object.entries(metricSamples).map(([name, samples]) => [
					name,
					summarize(samples.map((sample) => sample.durationMs))
				])
			),
			peakTimelineNodes: Math.max(...allSamples.map((sample) => sample.nodeCount)),
			maxViewportChangesPerDrag: Math.max(
				0,
				...(metricSamples.drag ?? []).map((sample) => sample.viewportChangeDelta)
			),
			longTasks: summarize(longTasks)
		});
	}

	const report = {
		schemaVersion: 1,
		generatedAt: new Date().toISOString(),
		budgetP95Ms,
		maxTimelineNodes,
		interactionSamples,
		initialRenderSamples,
		dataset: { durationHours: 24, segments: 1_440, events: 600 },
		environment: {
			browser: browser?.version() ?? 'unknown',
			runtime: `${process.release.name} ${process.version}`,
			os: `${platform()} ${release()} ${arch()}`,
			cpu: cpu?.model ?? 'unknown',
			logicalCpuCount: cpus().length,
			memoryGiB: rounded(totalmem() / 1024 ** 3),
			hardwareClass: process.env.KEEPPEEK_TIMELINE_PERF_HARDWARE_CLASS ?? 'developer workstation'
		},
		viewports: viewportReports
	};
	await mkdir(reportDirectory, { recursive: true });
	await Promise.all([
		writeFile(`${reportDirectory}${reportLabel}.json`, `${JSON.stringify(report, null, 2)}\n`),
		writeFile(`${reportDirectory}${reportLabel}.md`, markdownReport(report))
	]);
	console.log(markdownReport(report));

	const failures: string[] = [];
	for (const viewport of viewportReports) {
		for (const [metric, summary] of Object.entries(viewport.metrics)) {
			if (summary.p95Ms > budgetP95Ms) {
				failures.push(`${viewport.name} ${metric} p95 ${summary.p95Ms}ms > ${budgetP95Ms}ms`);
			}
		}
		if (viewport.peakTimelineNodes > maxTimelineNodes) {
			failures.push(
				`${viewport.name} peak nodes ${viewport.peakTimelineNodes} > ${maxTimelineNodes}`
			);
		}
		if (viewport.maxViewportChangesPerDrag > 2) {
			failures.push(
				`${viewport.name} drag triggered ${viewport.maxViewportChangesPerDrag} viewport updates`
			);
		}
	}
	if (enforceBudget) expect(failures, failures.join('\n')).toEqual([]);
	else expect(viewportReports).toHaveLength(fixtures.length);
});
