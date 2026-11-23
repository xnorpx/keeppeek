import { afterEach, expect, it, vi } from 'vitest';
import { BrowserLogStore } from './browser-logs';
import { ReportSession } from './report-session.svelte';

function setup() {
	let time = 100;
	const browser = new BrowserLogStore({ storage: null });
	const result = Promise.withResolvers<{
		reason: 'stopped' | 'display-ended';
		recording: Blob | null;
	}>();
	const diagnostics = new Blob(['diagnostics'], { type: 'application/gzip' });
	const capture = vi.fn(async () => ({
		result: result.promise,
		stop: () => result.promise
	}));
	const checkpoint = vi.fn(async (position: { utcMs: number; browserSequence: number }) => ({
		cursor: { ...position, server: null },
		server: null
	}));
	const createPackage = vi.fn(async () => diagnostics);
	const session = new ReportSession({
		now: () => time,
		latestBrowserSequence: () => browser.latestSequence,
		browserEntries: () => browser.snapshot(),
		package: createPackage,
		snapshot: checkpoint,
		capture
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
		createPackage
	};
}

afterEach(() => vi.useRealTimers());

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
		})
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
