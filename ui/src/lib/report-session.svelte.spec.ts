import { afterEach, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { page } from 'vitest/browser';
import ReportBugWorkflow from './components/ReportBugWorkflow.svelte';
import type { ReportTransport } from './report-submission';
import { createDiagnosticRedactor } from './diagnostic-redaction';
import { BrowserLogStore } from './browser-logs';
import { ReportSession } from './report-session.svelte';
import { startReportCapture } from './report-capture';

type ReportDependencies = ConstructorParameters<typeof ReportSession>[0];

function setup() {
	let time = 100;
	const browser = new BrowserLogStore({ storage: null });
	const result = Promise.withResolvers<{
		reason: 'stopped' | 'display-ended';
		recording: Blob | null;
	}>();
	const redactText = createDiagnosticRedactor([
		{ value: 'fixture-private-name', replacement: 'camera-001' }
	]).text;
	const diagnostics = new Blob(['diagnostics'], { type: 'application/gzip' });
	const capture = vi.fn(async () => ({
		result: result.promise,
		stop: () => result.promise
	}));
	const checkpoint = vi.fn<ReportDependencies['snapshot']>(
		async (position: { utcMs: number; browserSequence: number }) => ({
			cursor: { ...position, server: null },
			server: null
		})
	);
	const createPackage = vi.fn<ReportDependencies['package']>(async () => ({
		diagnostics,
		redactText,
		warnings: []
	}));
	const audit = vi.fn();
	const session = new ReportSession({
		now: () => time,
		latestBrowserSequence: () => browser.latestSequence,
		browserEntries: () => browser.snapshot(),
		package: createPackage,
		snapshot: checkpoint,
		capture,
		audit
	});
	return {
		time: (value: number) => {
			time = value;
		},
		session,
		browser,
		result,
		capture,
		checkpoint,
		createPackage,
		audit
	};
}

afterEach(() => vi.useRealTimers());

it('releases a real browser video track before the cancel or pagehide handler returns', async () => {
	const canvas = document.createElement('canvas');
	const stream = canvas.captureStream();
	const track = stream.getVideoTracks()[0];
	const selection = vi.spyOn(navigator.mediaDevices, 'getDisplayMedia').mockResolvedValue(stream);
	try {
		const owner = new AbortController();
		const capture = await startReportCapture({ signal: owner.signal });
		expect(track.readyState).toBe('live');
		owner.abort();
		expect(track.readyState).toBe('ended');
		expect(await capture.result).toEqual({ reason: 'cancelled', recording: null });
	} finally {
		track.stop();
		selection.mockRestore();
	}
});

it('keeps one logs-only session and packages only recorded cursor ranges', async () => {
	const { session, time, browser, capture, createPackage } = setup();
	browser.append('info', 'test', ['before']);
	session.start(false);
	session.start(true);
	browser.append('info', 'test', ['inside']);
	time(300);
	const stopped = session.stop();
	browser.append('info', 'test', ['outside']);
	await stopped;
	expect(capture).not.toHaveBeenCalled();
	expect(createPackage).toHaveBeenCalledExactlyOnceWith(
		null,
		expect.objectContaining({
			browserEntries: [expect.objectContaining({ message: 'inside' })]
		}),
		expect.any(AbortSignal)
	);
	expect(session.review?.window.browserCursors).toEqual({ start: 1, stop: 2 });
	expect(session.review?.window.warnings).toContain('server-logs-unavailable');
	expect(session.phase).toBe('review');
});

it('continues with logs when display permission is denied and reports unavailable packaging', async () => {
	const { session, capture, createPackage } = setup();
	capture.mockRejectedValueOnce(new DOMException('Permission denied', 'NotAllowedError'));
	createPackage.mockRejectedValueOnce(new Error('scrub context unavailable'));
	session.start(true);
	await Promise.resolve();
	expect(session.mediaState).toBe('unavailable');
	await session.stop();
	expect(session.review).toMatchObject({
		diagnostics: null,
		diagnosticsUnavailable: true,
		recording: null
	});
});

it('automatically finalizes a browser-ended display recording and lets the reviewer discard it', async () => {
	const { session, result } = setup();
	session.start(true);
	await Promise.resolve();
	const recording = new Blob(['private'], { type: 'video/webm' });
	result.resolve({ reason: 'display-ended', recording });
	await vi.waitFor(() => expect(session.phase).toBe('review'));
	expect(session.review).toMatchObject({ stopReason: 'display-ended', recording });
	session.discardRecording();
	expect(session.review?.recording).toBeNull();
});

it('cancels asynchronous finalization and clears the report after the hard time limit', async () => {
	const { session, checkpoint, createPackage } = setup();
	const delayed = Promise.withResolvers<Awaited<ReturnType<typeof checkpoint>>>();
	vi.useFakeTimers();
	session.start(false);
	checkpoint.mockReturnValueOnce(delayed.promise);
	await vi.advanceTimersByTimeAsync(5 * 60_000);
	expect(session.phase).toBe('finishing');
	session.cancel();
	delayed.resolve({ cursor: { utcMs: 500, browserSequence: 0, server: null }, server: null });
	await vi.advanceTimersByTimeAsync(0);
	expect(session.phase).toBe('idle');
	expect(session.review).toBeNull();
	expect(createPackage).not.toHaveBeenCalled();
});

it('shows the exact scrubbed issue text, included log counts and retention before contacting the service', async () => {
	const { session, browser, audit } = setup();
	browser.append('info', 'test', ['outside']);
	session.start(false);
	browser.append('info', 'test', ['inside']);
	await session.stop();
	const grant = { id: 'one', token: 'private-test-grant' };
	const transport: ReportTransport = {
		create: vi.fn(async () => grant),
		status: vi.fn(async () => ({ state: 'pending' as const })),
		upload: vi.fn(async (_grant, _kind, artifact, _signal, onBytes) => onBytes(artifact.size)),
		finalize: vi.fn(async () => ({
			state: 'complete' as const,
			issueUrl: 'https://github.com/xnorpx/keeppeek/issues/999'
		})),
		cancel: vi.fn(async () => {})
	};
	render(ReportBugWorkflow, {
		session,
		reporting: { transport, retentionDays: 30 },
		onaudit: audit
	});
	await page.getByLabelText('Issue title').fill('Test fixture-private-name');
	await page.getByLabelText('Problem description').fill('Playback stopped at 192.0.2.5.');
	await page.getByLabelText('Steps to reproduce').fill('Open a recording.');
	await page.getByLabelText('Expected behavior').fill('The clip plays.');
	await page.getByLabelText('Actual behavior').fill('The clip remains paused.');
	await page.getByRole('button', { name: 'Review submission' }).click();
	const confirm = page.getByRole('button', { name: 'Confirm and submit' });
	await expect.element(confirm).toBeVisible();
	await expect.element(page.getByText('1 browser and 0 server log entries')).toBeVisible();
	await expect.element(page.getByText('Test camera-001', { exact: true })).toBeVisible();
	await expect
		.element(page.getByText('Playback stopped at [REDACTED_IP].', { exact: true }))
		.toBeVisible();
	expect(page.getByRole('dialog').element().textContent).not.toContain('fixture-private-name');
	expect(transport.create).not.toHaveBeenCalled();
	await page.getByRole('button', { name: 'Back to review' }).click();
	await expect.element(page.getByLabelText('Issue title')).toHaveValue('Test fixture-private-name');
	await page.getByRole('button', { name: 'Review submission' }).click();
	expect(page.getByRole('dialog').element().textContent?.replace(/\s+/g, ' ')).toContain(
		'for up to 30 days'
	);
	expect(transport.create).not.toHaveBeenCalled();
	await confirm.click();
	await expect
		.element(page.getByRole('link', { name: 'Open the created GitHub issue' }))
		.toHaveAttribute('href', 'https://github.com/xnorpx/keeppeek/issues/999');
	await page.getByRole('button', { name: 'Done', exact: true }).click();
	expect(audit.mock.calls.map(([entry]) => entry.event)).toEqual(['start', 'complete']);
	expect(transport.create).toHaveBeenCalledExactlyOnceWith(
		expect.objectContaining({
			title: 'Test camera-001',
			description: 'Playback stopped at [REDACTED_IP].',
			reproduction: 'Open a recording.',
			expected: 'The clip plays.',
			actual: 'The clip remains paused.'
		}),
		expect.any(String),
		expect.any(AbortSignal)
	);
});

it('bounds stalled checkpoints and packaging while retaining finalized media', async () => {
	vi.useFakeTimers();
	const { session, result, checkpoint, createPackage } = setup();
	const stalledCheckpoint = Promise.withResolvers<Awaited<ReturnType<typeof checkpoint>>>();
	checkpoint.mockReturnValue(stalledCheckpoint.promise);
	const stalledPackage = Promise.withResolvers<Awaited<ReturnType<typeof createPackage>>>();
	createPackage.mockReturnValue(stalledPackage.promise);
	session.start(true);
	await Promise.resolve();
	const finalizing = session.stop();
	const recording = new Blob(['recording'], { type: 'video/webm' });
	result.resolve({ reason: 'stopped', recording });
	await vi.advanceTimersByTimeAsync(5_000);
	expect(createPackage).toHaveBeenCalledTimes(1);
	expect(checkpoint.mock.calls.map(([, signal]) => signal.aborted)).toEqual([true, true]);
	await vi.advanceTimersByTimeAsync(8_000);
	await finalizing;
	expect(createPackage.mock.calls[0]?.[2].aborted).toBe(true);
	expect(session.phase).toBe('review');
	expect(session.review?.recording).toBe(recording);
	expect(session.review?.diagnostics).toBeNull();
	expect(session.review?.evidenceWarnings).toEqual([
		'server-logs-unavailable',
		'diagnostics-package-unavailable'
	]);
	stalledPackage.resolve({ diagnostics: new Blob(['late']), redactText: String, warnings: [] });
	await Promise.resolve();
	expect(session.review?.diagnostics).toBeNull();
});

it('includes missing package sections in the review warnings when a scrubbed package is available', async () => {
	const { session, createPackage } = setup();
	const diagnostics = new Blob(['package']);
	createPackage.mockResolvedValue({
		diagnostics,
		redactText: String,
		warnings: ['diagnostics-metrics-unavailable']
	});
	session.start(false);
	await session.stop();
	expect(session.review?.evidenceWarnings).toEqual([
		'server-logs-unavailable',
		'diagnostics-metrics-unavailable'
	]);
	expect(session.review?.diagnostics).toBe(diagnostics);
});
