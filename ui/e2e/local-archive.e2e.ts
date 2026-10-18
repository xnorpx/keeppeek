import { expect, test, type Locator, type Page, type Route, type TestInfo } from '@playwright/test';
import { writeFile } from 'node:fs/promises';
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

function isolatedBackendRoute(origin: string, backend: string) {
	return async (route: Route) => {
		const url = new URL(route.request().url());
		if (
			url.origin === origin &&
			(url.pathname === '/create' ||
				url.pathname.startsWith('/api/') ||
				url.pathname.startsWith('/auth/') ||
				url.pathname.startsWith('/config/') ||
				url.pathname === '/metrics' ||
				url.pathname === '/logs')
		) {
			await route.fulfill({
				response: await route.fetch({ url: `${backend}${url.pathname}${url.search}` })
			});
		} else await route.continue();
	};
}

async function selectSeededRecording(archive: Locator, objectId: string) {
	await archive.getByRole('button', { name: 'Load archive source volumes' }).click();
	const sources = archive.getByRole('combobox', { name: 'Source volume', exact: true });
	await expect(sources.locator('option').nth(1)).toBeAttached();
	const volumes = await sources
		.locator('option')
		.evaluateAll((options) =>
			options
				.map((option) => (option instanceof HTMLOptionElement ? option.value : ''))
				.filter(Boolean)
		);
	expect(volumes.length).toBeGreaterThan(0);
	for (const volume of volumes.slice(0, 16)) {
		await sources.selectOption(volume);
		const load = archive.getByRole('button', { name: 'Load eligible media' });
		await load.click();
		await expect(load).toBeEnabled();
		const objects = archive.getByRole('combobox', { name: 'Media object', exact: true });
		if (!(await objects.count())) continue;
		const value = await objects
			.locator('option')
			.evaluateAll(
				(options, id) =>
					options
						.find(
							(option) => option instanceof HTMLOptionElement && option.value.endsWith(`:${id}`)
						)
						?.getAttribute('value'),
				objectId
			);
		if (!value) continue;
		await objects.selectOption(value);
		return value.slice(value.indexOf(':') + 1);
	}
	throw new Error('No seeded finalized recording appeared in the isolated catalog.');
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

async function configureArchive(
	page: Page,
	archive: Locator,
	fixture: Awaited<ReturnType<typeof startHomeAssistantServer>>,
	info: TestInfo
) {
	await page.goto('/settings#storage');
	await expect(archive).toContainText('Archive runtime unavailable.');
	await archive.getByRole('button', { name: 'Edit archive settings' }).click();
	await archive.getByLabel('Archive folder', { exact: true }).fill(fixture.archiveRoot);
	await archive.getByLabel('Enable local archive', { exact: true }).check();
	await captureSetupThemes(info, page, archive);
	await archive.getByRole('button', { name: 'Save archive settings', exact: true }).click();
	await expect(archive).toContainText('Archive settings saved. Restart to apply them.');
	await fixture.restart();
	await reportBackendProcess(info, fixture);
	await page.reload();
	await archive.getByRole('button', { name: 'Test archive destination' }).click();
	await expect(async () => {
		await archive.getByRole('button', { name: 'Refresh archive jobs' }).click();
		await expect(archive).toContainText('Archive destination is available.', { timeout: 1000 });
	}).toPass({ timeout: 20_000 });
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
	const objectId = await selectSeededRecording(archive, fixture.seededObjectId);
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
	await reportBackendProcess(info, fixture);
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
	const routeBackend = isolatedBackendRoute(new URL(baseURL).origin, fixture.url);
	await context.route('**/*', routeBackend);
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
	} finally {
		try {
			await attachArchiveEvidence(info, page, fixture, evidence, context.browser()?.version());
			await context.unroute('**/*', routeBackend);
		} finally {
			await live.close();
			await fixture.close();
		}
	}
});
