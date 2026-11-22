import {
	expect,
	test,
	type BrowserContext,
	type Locator,
	type Page,
	type TestInfo
} from '@playwright/test';
import { mkdir, readdir, readFile, rename, stat, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import {
	cardAdminKey,
	cardSourceIds,
	startHomeAssistantServer
} from './fixtures/home-assistant-server';
import {
	installArchiveVideoProbe,
	liveFrames,
	measureArchivePlayback
} from './fixtures/archive-playback';
import type { RecordingCoverageResponse } from '../src/lib/types';

const archiveViewports = [
	{ name: 'desktop', width: 1280, height: 900 },
	{ name: 'mobile', width: 390, height: 844 }
];

async function routeIsolatedBackend(context: BrowserContext, origin: string, backend: string) {
	await context.route(
		(url) =>
			url.origin === origin &&
			(url.pathname === '/create' ||
				url.pathname === '/delete' ||
				url.pathname.startsWith('/api/') ||
				url.pathname.startsWith('/auth/') ||
				url.pathname.startsWith('/config/') ||
				url.pathname === '/metrics' ||
				url.pathname === '/logs'),
		(route) => {
			const url = new URL(route.request().url());
			return route.continue({ url: backend + url.pathname + url.search });
		}
	);
}

async function selectSeededRecording(archive: Locator, objectId: string): Promise<boolean> {
	const picker = archive.getByRole('region', { name: 'Copy media to archive', exact: true });
	await picker.getByRole('button', { name: 'Load archive source volumes' }).click();
	const sources = picker.getByRole('combobox', { name: 'Source volume', exact: true });
	await expect(sources.locator('option').nth(1)).toBeAttached();
	const volumes = await sources
		.locator('option')
		.evaluateAll((options) =>
			options
				.map((option) => (option instanceof HTMLOptionElement ? option.value : ''))
				.filter(Boolean)
		);
	expect(volumes.length).toBeGreaterThan(0);
	for (const volume of volumes) {
		await sources.selectOption(volume);
		const load = picker.getByRole('button', { name: 'Load eligible media' });
		await load.click();
		for (let page = 0; page < 16; page++) {
			await expect(load).toBeEnabled();
			await expect(picker.getByRole('alert')).toHaveCount(0);
			const objects = picker.getByRole('combobox', { name: 'Media object', exact: true });
			const option = objects.getByRole('option').filter({ hasText: objectId });
			if (await option.count()) {
				const value = await option.getAttribute('value');
				if (!value?.endsWith(`:${objectId}`))
					throw new Error('Unexpected archive candidate identity');
				await objects.selectOption(value);
				return true;
			}
			const next = picker.getByRole('button', { name: 'Next media page' });
			if (!(await next.count())) break;
			if (page === 15) throw new Error('Isolated fixture exceeded 16 archive inventory pages');
			await next.click();
		}
	}
	return false;
}

async function refreshUntil(archive: Locator, job: Locator, text: string) {
	let latest = '';
	await expect(async () => {
		await archive.getByRole('button', { name: 'Refresh archive jobs' }).click();
		latest = await job.innerText();
		expect(latest.includes(text) || latest.includes('· failed')).toBe(true);
	}).toPass({ timeout: 30_000, intervals: [250, 500, 1000] });
	expect(latest).toContain(text);
}

async function captureResponsive(info: TestInfo, page: Page, surface: Locator, name: string) {
	const previous = page.viewportSize();
	try {
		for (const viewport of archiveViewports) {
			await page.setViewportSize(viewport);
			if (viewport.width === 390 && name !== 'archive-playback')
				await expect
					.poll(() =>
						surface.evaluate((element) =>
							[...element.querySelectorAll('button')].every(
								(button) => button.getBoundingClientRect().height >= 44
							)
						)
					)
					.toBe(true);
			await surface.evaluate((element) => element.scrollIntoView({ block: 'start' }));
			const heading = surface.getByRole('heading').first();
			await expect((await heading.count()) ? heading : surface).toBeInViewport();
			expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(
				viewport.width
			);
			const path = info.outputPath(`${name}-${viewport.name}.png`);
			await page.screenshot({ path });
			await info.attach(`${name}-${viewport.name}.png`, {
				path,
				contentType: 'image/png'
			});
		}
	} finally {
		if (previous) await page.setViewportSize(previous);
	}
}

async function captureSetupThemes(info: TestInfo, page: Page, archive: Locator) {
	const original = (await page.getByRole('button', { name: 'Switch to light theme' }).isVisible())
		? 'dark'
		: 'light';
	try {
		for (const theme of ['dark', 'light']) {
			const toggle = page.getByRole('button', { name: `Switch to ${theme} theme` });
			if (await toggle.isVisible()) await toggle.click();
			await captureResponsive(info, page, archive, `archive-setup-${theme}`);
		}
	} finally {
		const toggle = page.getByRole('button', { name: `Switch to ${original} theme` });
		if (await toggle.isVisible()) await toggle.click();
	}
}

async function probeDestination(archive: Locator, online: boolean) {
	const message = online
		? 'Archive destination is available.'
		: 'Archive destination unavailable. Recording continues';
	await archive.getByRole('button', { name: 'Test archive destination' }).click();
	await expect(async () => {
		await archive.getByRole('button', { name: 'Refresh archive jobs' }).click();
		await expect(archive).toContainText(message, { timeout: 1000 });
	}).toPass({ timeout: 20_000 });
}

async function configureArchive(
	page: Page,
	archive: Locator,
	fixture: Awaited<ReturnType<typeof startHomeAssistantServer>>,
	info?: TestInfo
) {
	await page.goto('/settings#storage');
	// The first navigation also compiles the settings route on the isolated Vite server.
	await expect(archive).toContainText('Archive runtime unavailable.', { timeout: 45_000 });
	await archive.getByRole('button', { name: 'Edit archive settings' }).click();
	await archive.getByLabel('Archive folder', { exact: true }).fill(fixture.archiveRoot);
	await archive.getByLabel('Enable local archive', { exact: true }).check();
	if (info) await captureSetupThemes(info, page, archive);
	await archive.getByRole('button', { name: 'Save archive settings', exact: true }).click();
	await expect(archive).toContainText('Archive settings saved. Restart to apply them.');
	await fixture.restart();
	if (info) await reportBackendProcess(info, fixture);
	await page.reload();
	await probeDestination(archive, true);
}

async function reportBackendProcess(
	info: TestInfo,
	fixture: Awaited<ReturnType<typeof startHomeAssistantServer>>
) {
	const process = { pid: fixture.backendProcessId(), observedAt: new Date().toISOString() };
	await writeFile(info.outputPath('archive-backend-process.json'), JSON.stringify(process));
	console.log(`[archive-fixture] backend PID ${process.pid}`);
}

async function recordingProgress(page: Page, backend: string) {
	const response = await page.request.get(
		`${backend}/recording-coverage?search=${encodeURIComponent(cardSourceIds[1])}`,
		{ headers: { Authorization: `Bearer ${cardAdminKey}` } }
	);
	expect(response.ok()).toBe(true);
	const coverage = (await response.json()) as RecordingCoverageResponse;
	const stream = coverage.cameras
		.find((camera) => camera.camera_id === cardSourceIds[1])
		?.streams.find((stream) => stream.recording_requested);
	return {
		observedAtMs: coverage.generated_at_ms,
		lastWriteAtMs: stream?.last_write_at_ms ?? 0,
		writerState: stream?.writer_state ?? 'missing',
		recordingBytes: stream?.recording_bytes ?? 0
	};
}

async function continuitySnapshot(live: Page, backend: string) {
	await expect.poll(() => liveFrames(live), { timeout: 20_000 }).toBeGreaterThan(0);
	await expect
		.poll(async () => (await recordingProgress(live, backend)).lastWriteAtMs, { timeout: 20_000 })
		.toBeGreaterThan(0);
	return { frames: await liveFrames(live), ...(await recordingProgress(live, backend)) };
}

async function expectContinuity(
	live: Page,
	backend: string,
	before: Awaited<ReturnType<typeof continuitySnapshot>>
) {
	await expect.poll(() => liveFrames(live), { timeout: 20_000 }).toBeGreaterThan(before.frames);
	await expect
		.poll(async () => (await recordingProgress(live, backend)).lastWriteAtMs, { timeout: 20_000 })
		.toBeGreaterThan(before.lastWriteAtMs);
	return { before, after: await continuitySnapshot(live, backend) };
}

type ArchiveFixture = Awaited<ReturnType<typeof startHomeAssistantServer>>;
type ArchiveEvidence = {
	timings: Awaited<ReturnType<typeof measureArchivePlayback>>[];
	transfers: { operation: string; elapsed_ms: number; object_bytes: number }[];
	continuity: { operation: string; evidence: Awaited<ReturnType<typeof expectContinuity>> }[];
};

async function copyAndEvict(
	page: Page,
	live: Page,
	fixture: ArchiveFixture,
	info: TestInfo,
	evidence: ArchiveEvidence
) {
	await page.goto('/settings#storage');
	await live.goto('/');
	const archive = page.getByRole('region', { name: 'Local archive', exact: true });
	const baseline = await continuitySnapshot(live, fixture.url);
	const objectId = fixture.seededObjectId;
	expect(await selectSeededRecording(archive, objectId)).toBe(true);
	const startedAt = performance.now();
	await archive.getByRole('button', { name: 'Queue archive copy' }).click();
	const job = archive.getByRole('article').first();
	await refreshUntil(archive, job, '· ready');
	evidence.transfers.push({
		operation: 'copy',
		elapsed_ms: performance.now() - startedAt,
		object_bytes: fixture.seededObjectBytes
	});
	await captureResponsive(info, page, archive, 'archive-jobs');
	evidence.continuity.push({
		operation: 'copy',
		evidence: await expectContinuity(live, fixture.url, baseline)
	});
	await job.getByRole('button', { name: `Evict primary ${objectId}` }).click();
	const eviction = page.getByRole('dialog', { name: 'Evict primary copy?' });
	await expect(eviction).toContainText(objectId);
	await expect(eviction.getByRole('button', { name: 'Cancel', exact: true })).toBeFocused();
	await captureResponsive(info, page, eviction, 'archive-eviction-confirmation');
	await eviction.getByRole('button', { name: 'Evict primary', exact: true }).click();
	await refreshUntil(archive, job, 'Primary evicted; archive retained.');
}

async function observeArchivedPlayback(
	page: Page,
	fixture: ArchiveFixture,
	info: TestInfo,
	evidence: ArchiveEvidence
) {
	await fixture.restart();
	await reportBackendProcess(info, fixture);
	evidence.timings.push(
		await measureArchivePlayback(page, fixture.seededAtMs + 5000, 'archive-initial')
	);
	await captureResponsive(
		info,
		page,
		page.getByRole('region', { name: 'Recorded video player' }),
		'archive-playback'
	);
	evidence.timings.push(
		await measureArchivePlayback(page, fixture.seededAtMs + 5000, 'archive-repeat')
	);
}

async function restorePrimary(
	page: Page,
	live: Page,
	fixture: ArchiveFixture,
	info: TestInfo,
	evidence: ArchiveEvidence
) {
	await page.goto('/settings#storage');
	await live.reload();
	const archive = page.getByRole('region', { name: 'Local archive', exact: true });
	const job = archive.getByRole('article').first();
	const baseline = await continuitySnapshot(live, fixture.url);
	await expect(job).toContainText('Primary evicted; archive retained.');
	await job
		.getByRole('button', { name: `Preview archive deletion ${fixture.seededObjectId}` })
		.click();
	const blocked = page.getByRole('dialog', { name: 'Archive deletion preview' });
	await expect(blocked).toContainText('Restore the primary copy first');
	await expect(blocked.getByRole('button', { name: 'Confirm archive deletion' })).toHaveCount(0);
	await blocked.getByRole('button', { name: 'Close', exact: true }).click();
	await job.getByRole('button', { name: `Restore primary ${fixture.seededObjectId}` }).click();
	const startedAt = performance.now();
	await page
		.getByRole('dialog', { name: 'Restore primary copy?' })
		.getByRole('button', { name: 'Restore primary', exact: true })
		.click();
	await refreshUntil(archive, job, 'Primary copy retained.');
	evidence.transfers.push({
		operation: 'restore',
		elapsed_ms: performance.now() - startedAt,
		object_bytes: fixture.seededObjectBytes
	});
	evidence.continuity.push({
		operation: 'restore',
		evidence: await expectContinuity(live, fixture.url, baseline)
	});
	await fixture.restart();
	if (info) await reportBackendProcess(info, fixture);
	await page.reload();
	await expect(job).toContainText('Primary copy retained.');
}

async function attachArchiveEvidence(
	info: TestInfo,
	page: Page,
	fixture: ArchiveFixture,
	evidence: ArchiveEvidence,
	browserVersion: string | undefined
) {
	const observationsPath = info.outputPath('archive-playback-observations.json');
	await writeFile(
		observationsPath,
		JSON.stringify(
			{
				browserVersion,
				originalViewport: page.viewportSize(),
				capturedViewports: archiveViewports,
				timingScope:
					'Browser navigation/loadstart and UI seek to requestVideoFrameCallback presentation; observed values, no performance thresholds.',
				transferScope:
					'UI submission to first observed ready/retained state, including control and refresh polling overhead; object bytes from seeded file metadata.',
				...evidence
			},
			null,
			2
		)
	);
	await info.attach('archive-playback-observations.json', {
		path: observationsPath,
		contentType: 'application/json'
	});
	const logsPath = info.outputPath('isolated-archive-server.log');
	await writeFile(logsPath, fixture.logs());
	await info.attach('isolated-archive-server.log', { path: logsPath, contentType: 'text/plain' });
	await fixture.preserveArchiveEvidence(info.outputPath('isolated-fixture-metadata'));
}

test('local archive survives restart and plays evicted media before restoring its primary', async ({
	page,
	context,
	baseURL
}, info) => {
	test.setTimeout(180_000);
	if (!baseURL) throw new Error('Archive lifecycle requires the configured UI origin.');
	const fixture = await startHomeAssistantServer(new URL(baseURL).origin, {
		trustedLocal: true,
		archiveFixture: true
	});
	await routeIsolatedBackend(context, new URL(baseURL).origin, fixture.url);
	await installArchiveVideoProbe(page);
	const live = await context.newPage();
	const evidence: ArchiveEvidence = { timings: [], transfers: [], continuity: [] };
	try {
		await reportBackendProcess(info, fixture);
		const archive = page.getByRole('region', { name: 'Local archive', exact: true });
		await configureArchive(page, archive, fixture, info);
		evidence.timings.push(await measureArchivePlayback(page, fixture.seededAtMs + 5000, 'local'));
		await copyAndEvict(page, live, fixture, info, evidence);
		await observeArchivedPlayback(page, fixture, info, evidence);
		await restorePrimary(page, live, fixture, info, evidence);
		evidence.timings.push(
			await measureArchivePlayback(page, fixture.seededAtMs + 5000, 'local-restored')
		);
		await page.goto('/settings#storage');
		const job = archive.getByRole('article').first();
		await job
			.getByRole('button', { name: `Preview archive deletion ${fixture.seededObjectId}` })
			.click();
		const deletion = page.getByRole('dialog', { name: 'Archive deletion preview' });
		await expect(deletion).toContainText('1 selected');
		await expect(deletion).toContainText(fixture.seededObjectId);
		const beforeDeletion = await continuitySnapshot(live, fixture.url);
		const startedAt = performance.now();
		await deletion.getByRole('button', { name: 'Confirm archive deletion' }).click();
		await expect(deletion).toContainText('Batch confirmed');
		await deletion.getByRole('button', { name: 'Close', exact: true }).click();
		const deletionCompleted = process.platform === 'win32';
		const outcome = deletionCompleted
			? 'delete archive replica'
			: 'retain archive with mutable primary';
		await refreshUntil(
			archive,
			job,
			deletionCompleted
				? '· deleted'
				: 'Archive retained. The primary needs filesystem-enforced write protection'
		);
		evidence.transfers.push({
			operation: outcome,
			elapsed_ms: performance.now() - startedAt,
			object_bytes: fixture.seededObjectBytes
		});
		evidence.continuity.push({
			operation: outcome,
			evidence: await expectContinuity(live, fixture.url, beforeDeletion)
		});
		expect(await selectSeededRecording(archive, fixture.seededObjectId)).toBe(false);
		evidence.timings.push(
			await measureArchivePlayback(
				page,
				fixture.seededAtMs + 5000,
				deletionCompleted ? 'local-after-archive-deletion' : 'local-after-blocked-deletion'
			)
		);
	} finally {
		try {
			await attachArchiveEvidence(info, page, fixture, evidence, context.browser()?.version());
		} finally {
			await live.close();
			await fixture.close();
		}
	}
});

test('live, recording and primary timeline playback continue when the bound archive disappears or is replaced', async ({
	page,
	context,
	baseURL
}, info) => {
	test.setTimeout(180_000);
	if (!baseURL) throw new Error('Archive outage coverage requires the configured UI origin.');
	const fixture = await startHomeAssistantServer(new URL(baseURL).origin, {
		trustedLocal: true,
		archiveFixture: true
	});
	const root = fixture.archiveRoot;
	const live = await context.newPage();
	const evidence = [];
	try {
		await routeIsolatedBackend(context, new URL(baseURL).origin, fixture.url);
		await installArchiveVideoProbe(page);
		const archive = page.getByRole('region', { name: 'Local archive', exact: true });
		await configureArchive(page, archive, fixture);
		await live.goto('/');
		const pid = fixture.backendProcessId();
		for (const mode of ['missing', 'replaced'] as const) {
			await page.goto('/settings#storage');
			const before = await continuitySnapshot(live, fixture.url);
			const detached = root + '.detached';
			await rename(root, detached);
			try {
				if (mode === 'replaced') {
					await mkdir(root);
					await writeFile(join(root, 'unrelated.txt'), 'keep this file');
				}
				await probeDestination(archive, false);
				const playback = await measureArchivePlayback(page, fixture.seededAtMs + 5000, 'local');
				const continuity = await expectContinuity(live, fixture.url, before);
				expect(fixture.backendProcessId()).toBe(pid);
				if (mode === 'missing') {
					await expect(stat(root)).rejects.toMatchObject({ code: 'ENOENT' });
				} else {
					expect(await readdir(root)).toEqual(['unrelated.txt']);
					expect(await readFile(join(root, 'unrelated.txt'), 'utf8')).toBe('keep this file');
				}
				evidence.push({ mode, playback, continuity });
			} finally {
				if (mode === 'replaced') await rename(root, root + '.replacement');
				await rename(detached, root);
			}
			await page.goto('/settings#storage');
			await probeDestination(archive, true);
		}
		expect(fixture.backendProcessId()).toBe(pid);
		await info.attach('isolated-target-outages', {
			contentType: 'application/json',
			body: JSON.stringify(evidence)
		});
	} finally {
		try {
			await info.attach('isolated-archive-outage-server.log', {
				contentType: 'text/plain',
				body: fixture.logs()
			});
		} finally {
			await live.close();
			await fixture.close();
		}
	}
});
