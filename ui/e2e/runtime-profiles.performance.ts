import { expect, test } from '@playwright/test';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { cpus, platform, release } from 'node:os';
import { resolve } from 'node:path';
import { startHomeAssistantServer } from './fixtures/home-assistant-server';
import { isolatedBackendRoute } from './fixtures/isolated-backend-route';

function observeOperator() {
	const revisions = new Map<string, { name: string; time: number }>();
	const tasks: Array<{ start: number; duration: number }> = [];
	const el = document.querySelector('[aria-label="Active runtime profile"] p')!;
	const collect = () => {
		const revision = el.textContent.match(/revision\s+(\d+)/)?.[1];
		const name = el.querySelector('strong')?.textContent.trim();
		if (revision && name && !revisions.has(revision))
			revisions.set(revision, { name, time: performance.timeOrigin + performance.now() });
	};
	const observer = new MutationObserver(collect);
	observer.observe(el, { childList: true, characterData: true, subtree: true });
	collect();
	if (!PerformanceObserver.supportedEntryTypes.includes('longtask'))
		throw new Error('Chromium long-task timing is required.');
	const record = (entries: PerformanceEntry[]) => {
		for (const entry of entries)
			tasks.push({ start: performance.timeOrigin + entry.startTime, duration: entry.duration });
	};
	const perf = new PerformanceObserver((list) => record(list.getEntries()));
	perf.observe({ type: 'longtask' });
	return {
		lookup: (revision: string) => revisions.get(revision),
		finish() {
			record(perf.takeRecords());
			perf.disconnect();
			observer.disconnect();
			return tasks;
		}
	};
}

async function capture<T>(measurement: Promise<T>) {
	try {
		return { value: await measurement };
	} catch (error) {
		return { error: error instanceof Error ? error.message : String(error) };
	}
}

function summary(values: number[]) {
	const sorted = [...values].sort((a, b) => a - b);
	const percentile = (p: number) => sorted[Math.ceil(sorted.length * p) - 1]!;
	return { median: percentile(0.5), p95: percentile(0.95), max: percentile(1) };
}

test('durable activation, connected Settings revision and camera convergence', async ({
	page,
	browser,
	baseURL
}, info) => {
	const smoke = process.env.KEEPPEEK_PROFILE_SMOKE === '1';
	const size = smoke ? { cameras: 4, profiles: 2 } : { cameras: 127, profiles: 32 };
	const count = smoke ? 2 : 30;
	const directory =
		process.env.KEEPPEEK_PROFILE_RELEASE_DIR ??
		resolve(import.meta.dirname, '../../target/release');
	const exe = platform() === 'win32' ? 'keeppeek.exe' : 'keeppeek';
	const sha256 = createHash('sha256')
		.update(await readFile(resolve(directory, exe)))
		.digest('hex');
	const fixture = await startHomeAssistantServer(baseURL!, {
		trustedLocal: true,
		profileCapacity: size,
		releaseDirectory: directory
	});
	const operator = await browser.newPage();
	const convergence = [];
	const samples = [];
	try {
		await operator.goto(fixture.url + '/settings#runtime-profiles');
		await expect(operator.getByText(/Active policy: Base/)).toBeVisible({ timeout: 60_000 });
		await page.route('**/*', isolatedBackendRoute(baseURL!, fixture.url));
		await page.route('**/timing-client', (route) =>
			route.fulfill({ contentType: 'text/html', body: '<title>Profile timing client</title>' })
		);
		await page.goto('/timing-client');
		const client = await page.evaluateHandle(async (size) => {
			const path = '/e2e/fixtures/runtime-profiles-browser.ts';
			const fixture = (await import(path)) as typeof import('./fixtures/runtime-profiles-browser');
			return fixture.measureProfileActivations(size);
		}, size);
		try {
			const ui = await operator.evaluateHandle(observeOperator);
			for (let index = 0; index < count; index++) {
				const change = await client.evaluate((peer, i) => peer.activate(i), index);
				const [screen, physical] = await Promise.all([
					capture(
						(async () => {
							await expect
								.poll(
									async () =>
										(await ui.evaluate((view, rev) => view.lookup(rev), change.revision))?.name
								)
								.toBe('Qualification ' + index);
							return (await ui.evaluate((view, rev) => view.lookup(rev), change.revision))!.time;
						})()
					),
					capture(client.evaluate((peer, revision) => peer.convergence(revision), change.revision))
				]);
				samples.push({
					...change,
					shown: screen.value,
					screenError: screen.error,
					ackWatchMs: Math.max(change.ack, change.watch) - change.start,
					desiredMs: screen.value === undefined ? null : screen.value - change.start
				});
				convergence.push({
					...physical,
					expectedRevision: change.revision,
					observedByMs: physical.value ? physical.value.observed - change.start : null
				});
			}
			const tasks = await ui.evaluate((view) => view.finish());
			const ackWatch = summary(samples.map((s) => s.ackWatchMs));
			const measured = samples.flatMap((s) => (s.desiredMs === null ? [] : [s.desiredMs]));
			const desired = measured.length === count ? summary(measured) : null;
			const report = {
				size,
				count,
				smoke,
				sha256,
				build: process.env.KEEPPEEK_PROFILE_BUILD_SHA,
				storage: process.env.KEEPPEEK_PROFILE_STORAGE,
				browser: browser.version(),
				os: platform() + ' ' + release(),
				cpu: cpus()[0]?.model,
				ackWatch,
				desired,
				longTasks: tasks,
				samples,
				convergence
			};
			await writeFile(info.outputPath('profiles.json'), JSON.stringify(report, null, 2));
			for (const state of convergence) {
				expect.soft(state.error).toBeUndefined();
				expect.soft(state.value?.revision).toBe(state.expectedRevision);
				expect.soft(state.value?.count).toBe(size.cameras);
				expect.soft(state.value?.pending).toEqual([]);
			}
			expect(measured.length).toBe(count);
			if (!smoke) {
				expect.soft(ackWatch.p95).toBeLessThanOrEqual(250);
				expect.soft(desired!.p95).toBeLessThanOrEqual(500);
				expect.soft(tasks.filter((task) => task.duration > 50)).toHaveLength(0);
			}
		} finally {
			await client.evaluate((peer) => peer.close());
		}
	} finally {
		try {
			await operator.close();
		} finally {
			await fixture.close();
		}
	}
});
