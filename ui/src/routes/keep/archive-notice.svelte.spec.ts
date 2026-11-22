import { page } from 'vitest/browser';
import { expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { create } from '@bufbuild/protobuf';
import { RecordedAudioTimingSchema, type RecordedAudioTiming } from '$lib/proto/webrtc_pb';
import '../../app.css';
import Keep from './+page.svelte';

const context = vi.hoisted(() => ({ control: vi.fn() }));
vi.mock('$lib/control-context', () => ({ useControlClient: context.control }));
vi.mock('$app/navigation', () => ({ afterNavigate: vi.fn(), goto: vi.fn() }));
vi.mock('$app/paths', () => ({ resolve: (value: string) => value }));
vi.mock('$app/state', () => ({
	page: { url: new URL('http://localhost/keep'), state: {} },
	navigating: {}
}));

const start = Date.parse('2026-10-15T12:00:00Z');
const recording = {
	stream: 'sub',
	date: '2026-10-15',
	hour: '12',
	filename: 'clip.mp4',
	url: 'stored:front:sub',
	start_time_ms: start,
	end_time_ms: start + 40_000,
	duration_ms: 40_000
};

function cursor(id: string) {
	let archive = (_value: boolean) => {};
	let failure = (_message: string) => {};
	let startup = (_event: { phase: 'metadata'; contentType: string }) => {};
	let timing = (_value: RecordedAudioTiming | undefined) => {};
	const url = URL.createObjectURL(new MediaSource());
	const playback = {
		id,
		sourceId: 'front',
		streamId: 'sub',
		url,
		anchorTimeMs: start,
		initialOffsetSeconds: 0,
		recordedAudio: undefined as RecordedAudioTiming | undefined,
		canSeekLocally: () => false,
		seek: vi.fn(async () => archive(true)),
		enterScrub: vi.fn(async () => {}),
		commitPlayback: vi.fn(async () => {}),
		setPlaybackRate: vi.fn(),
		setPlaying: vi.fn(),
		observe: vi.fn(),
		onKeyFrame: () => () => {},
		onRecordedAudio: (listener: typeof timing) => {
			timing = listener;
			listener(playback.recordedAudio);
			return () => {};
		},
		setRecordedAudioPreview: vi.fn(async (_offsetMs: number | null, _timestampMs: number) => {}),
		onError: (listener: typeof failure) => {
			failure = listener;
			return () => {};
		},
		onStartup: (listener: typeof startup) => {
			startup = listener;
			return () => {};
		},
		onArchive: (listener: typeof archive) => {
			archive = listener;
			listener(false);
			return () => {};
		},
		close: vi.fn(async () => {})
	};
	return {
		playback,
		emitArchive: (value: boolean) => archive(value),
		emitError: (message: string) => failure(message),
		emitMetadata: () => startup({ phase: 'metadata', contentType: 'video/mp4' }),
		emitTiming: (value: RecordedAudioTiming) => {
			playback.recordedAudio = value;
			timing(value);
		},
		dispose: () => URL.revokeObjectURL(url)
	};
}

function controller(
	first: ReturnType<typeof cursor>,
	next: ReturnType<typeof cursor>,
	audioSupported: boolean
) {
	const open = vi.fn().mockResolvedValueOnce(first.playback).mockResolvedValue(next.playback);
	context.control.mockReturnValue({
		onCapabilities: (listener: (values: string[]) => void) => {
			listener([
				'keeppeek.stored-media',
				...(audioSupported ? ['keeppeek.recorded-audio.v1'] : [])
			]);
			return () => {};
		},
		onAccessState: (listener: (value: { session: null }) => void) => {
			listener({ session: null });
			return () => {};
		},
		getCameras: async () => [
			{ id: 'front', name: 'Front', profiles: [{ stream: 'sub', encoding: 'H264' }] }
		],
		getHealth: async () => null,
		getRecordingDates: async () => ['2026-10-15'],
		getRecordingsInRange: async () => [{ segments: [recording], events: [] }],
		queryStoredTimeline: async () => ({ ranges: [], events: [] }),
		openStoredMedia: open
	});
	return open;
}

function keepFixture(audioSupported = false) {
	const original = window.location.href;
	const first = cursor('first');
	const next = cursor('replacement');
	const open = controller(first, next, audioSupported);
	return {
		first,
		next,
		open,
		async mount() {
			await page.viewport(1280, 800);
			window.history.replaceState(
				null,
				'',
				`/keep?camera=front&stream=sub&date=2026-10-15&at=${start}`
			);
			await render(Keep);
			await vi.waitFor(() => expect(open).toHaveBeenCalledOnce());
		},
		dispose() {
			window.history.replaceState(null, '', original);
			first.dispose();
			next.dispose();
		}
	};
}

function beginDrag() {
	const playhead = page.getByRole('button', { name: /Playback position at/ }).element();
	playhead.setPointerCapture = vi.fn();
	playhead.hasPointerCapture = vi.fn(() => true);
	playhead.releasePointerCapture = vi.fn();
	const send = (type: string) =>
		playhead.dispatchEvent(
			new PointerEvent(type, {
				bubbles: true,
				cancelable: true,
				button: 0,
				pointerId: 1,
				clientY: 80
			})
		);
	send('pointerdown');
	send('pointermove');
	return (type: 'pointerup' | 'pointercancel' = 'pointerup') => send(type);
}

it.each([true, false])(
	'offers explicit initial unadjusted recovery only with server support (%s)',
	async (supported) => {
		const { open, mount, dispose, next } = keepFixture(supported);
		open
			.mockReset()
			.mockRejectedValueOnce(
				new Error(
					'Recorded audio correction requires compatible continuous AAC media within the supported limits.'
				)
			)
			.mockResolvedValue(next.playback);
		try {
			await mount();
			await expect
				.element(page.getByRole('alert'))
				.toMatchTextContent(
					'Recorded audio correction requires compatible continuous AAC media within the supported limits.'
				);
			const recovery = page.getByRole('button', {
				name: 'Try unadjusted audio preview',
				exact: true
			});
			if (!supported) {
				await expect.element(recovery).not.toBeInTheDocument();
				expect(open).toHaveBeenCalledOnce();
				return;
			}
			await recovery.click();
			await vi.waitFor(() => expect(open).toHaveBeenCalledTimes(2));
			expect(open.mock.calls[1]?.[0]).toMatchObject({
				sourceId: 'front',
				streamId: 'sub',
				timestampMs: start,
				recordedAudioPreview: 0
			});
		} finally {
			dispose();
		}
	}
);

it('applies a viewer-authorized audio preview at the displayed moment and resets configured timing', async () => {
	const { first, open, mount, dispose } = keepFixture();
	const saved = create(RecordedAudioTimingSchema, {
		configuredOffsetMs: 250,
		appliedOffsetMs: 250,
		mappingVersion: 1
	});
	first.playback.setRecordedAudioPreview.mockImplementation(async (offsetMs) => {
		first.emitTiming({ ...saved, appliedOffsetMs: offsetMs ?? 250, preview: offsetMs !== null });
	});
	try {
		await mount();
		first.emitTiming(saved);
		await page.getByRole('button', { name: 'Recorded audio preview', exact: true }).click();
		await page.getByRole('spinbutton', { name: 'Preview offset (ms)' }).fill('0');
		await page.getByRole('button', { name: 'Apply preview', exact: true }).click();
		await vi.waitFor(() =>
			expect(first.playback.setRecordedAudioPreview).toHaveBeenCalledWith(0, start)
		);
		await page.getByRole('button', { name: 'Reset preview', exact: true }).click();
		await vi.waitFor(() =>
			expect(first.playback.setRecordedAudioPreview).toHaveBeenLastCalledWith(null, start)
		);
		expect(open).toHaveBeenCalledOnce();
		await page.getByRole('button', { name: 'Close', exact: true }).click();
		await expect.element(page.getByRole('dialog')).not.toBeInTheDocument();
	} finally {
		dispose();
	}
});

it('ignores a replaced cursor timing callback when opening the new audio preview', async () => {
	const { first, next, open, mount, dispose } = keepFixture();
	try {
		await mount();
		first.emitError('Recording source is unavailable.');
		await page.getByRole('button', { name: 'Close playback', exact: true }).click();
		await page.getByRole('button', { name: 'Refresh recordings', exact: true }).click();
		await vi.waitFor(() => expect(open).toHaveBeenCalledTimes(2));
		next.emitTiming(create(RecordedAudioTimingSchema, { appliedOffsetMs: 100, mappingVersion: 1 }));
		first.emitTiming(
			create(RecordedAudioTimingSchema, { appliedOffsetMs: 777, mappingVersion: 1 })
		);
		await page.getByRole('button', { name: 'Recorded audio preview', exact: true }).click();
		await expect
			.element(page.getByRole('spinbutton', { name: 'Preview offset (ms)' }))
			.toHaveValue(100);
	} finally {
		dispose();
	}
});

it('keeps the reported decoded mobile frame visible when playback fails', async () => {
	const { first, mount, dispose } = keepFixture();
	try {
		await mount();
		await page.viewport(390, 844);
		const video = document.querySelector('video');
		if (!video) throw new Error('Keep video is unavailable');
		const frame = video.closest<HTMLElement>('[data-keep-media-frame]');
		const zoomIn = frame?.querySelector<HTMLButtonElement>('button[aria-label="Digital zoom in"]');
		if (!frame || !zoomIn) throw new Error('Keep media controls are unavailable');
		await vi.waitFor(() => expect(frame.inert).toBe(true));
		await vi.waitFor(() => expect(zoomIn.disabled).toBe(false));
		zoomIn.focus();
		expect(document.activeElement).not.toBe(zoomIn);
		expect(video.checkVisibility()).toBe(true);
		expect(video.getBoundingClientRect().width).toBeGreaterThan(0);
		expect(video.getBoundingClientRect().height).toBeGreaterThan(0);
		Object.defineProperty(video, 'videoWidth', { configurable: true, value: 640 });
		Object.defineProperty(video, 'videoHeight', { configurable: true, value: 360 });
		video.dispatchEvent(new Event('resize'));
		video.dispatchEvent(new Event('loadeddata'));
		first.emitError('No first media fragment arrived.');
		await expect
			.element(page.getByRole('alert'))
			.toMatchTextContent('No first media fragment arrived.');
		await expect
			.element(page.getByRole('region', { name: 'Recorded video player' }))
			.toHaveAttribute('data-recording-empty-notice', 'false');
		await expect
			.element(page.getByRole('application', { name: 'Digital zoom viewport' }))
			.toBeVisible();
		expect(video.checkVisibility()).toBe(true);
		expect(frame.inert).toBe(false);
		zoomIn.focus();
		expect(document.activeElement).toBe(zoomIn);
	} finally {
		dispose();
	}
});

it('tracks the retained cursor across cold seeks and ignores a replaced cursor callback', async () => {
	const { first, open, mount, dispose } = keepFixture();
	try {
		await mount();
		const slider = page.getByRole('slider', { name: 'Recording position', exact: true });
		await expect.element(slider).toBeEnabled();
		await slider.click();
		await vi.waitFor(() => expect(first.playback.seek).toHaveBeenCalledOnce());
		await expect
			.element(page.getByRole('region', { name: 'Archive playback', exact: true }))
			.toBeVisible();
		await page.getByRole('button', { name: 'Close playback', exact: true }).click();
		await vi.waitFor(() => expect(first.playback.close).toHaveBeenCalledOnce());
		await page.getByRole('button', { name: 'Refresh recordings', exact: true }).click();
		await vi.waitFor(() => expect(open).toHaveBeenCalledTimes(2));
		await expect.element(page.getByRole('region', { name: 'Playback', exact: true })).toBeVisible();
		first.emitArchive(true);
		await expect
			.element(page.getByRole('region', { name: 'Archive playback', exact: true }))
			.not.toBeInTheDocument();
	} finally {
		dispose();
	}
});

it.each(['pointerup', 'pointercancel'] as const)(
	'reopens archive notices and retry after Close then %s',
	async (endEvent) => {
		const { first, next, open, mount, dispose } = keepFixture();
		try {
			await mount();
			await page.getByRole('button', { name: 'Close playback', exact: true }).click();
			const finish = beginDrag();
			await vi.waitFor(() => expect(next.playback.seek).toHaveBeenCalledOnce());
			await expect
				.element(page.getByRole('region', { name: 'Archive playback', exact: true }))
				.toBeVisible();
			vi.useFakeTimers();
			next.emitMetadata();
			await vi.advanceTimersByTimeAsync(4_000);
			await expect
				.element(page.getByText('Archive playback unavailable', { exact: true }))
				.not.toBeInTheDocument();
			vi.useRealTimers();
			finish(endEvent);
			await vi.waitFor(() => expect(next.playback.commitPlayback).toHaveBeenCalledOnce());
			vi.useFakeTimers();
			next.emitMetadata();
			await vi.advanceTimersByTimeAsync(4_000);
			await expect
				.element(page.getByText(/No recording initialization arrived within/))
				.toBeVisible();
			vi.useRealTimers();
			next.emitError('No first media fragment arrived.');
			await expect.element(page.getByText(/No first media fragment arrived/)).toBeVisible();
			await expect
				.element(page.getByRole('button', { name: 'Retry playback', exact: true }))
				.toBeEnabled();
			first.emitError('No stale cursor failure should be shown.');
			await expect.element(page.getByText(/No stale cursor failure/)).not.toBeInTheDocument();
			await page.getByRole('button', { name: 'Retry playback', exact: true }).click();
			await vi.waitFor(() => expect(open).toHaveBeenCalledTimes(3));
		} finally {
			vi.useRealTimers();
			dispose();
		}
	}
);

it('shows a rejected scrub open after Close and keeps its target retryable', async () => {
	const { open, mount, dispose } = keepFixture();
	try {
		await mount();
		await page.getByRole('button', { name: 'Close playback', exact: true }).click();
		open
			.mockRejectedValueOnce(new Error('Archive target is unavailable.'))
			.mockRejectedValueOnce(new Error('Archive target is unavailable.'));
		const finish = beginDrag();
		finish();
		await expect
			.element(page.getByText('Archive target is unavailable.', { exact: true }))
			.toBeVisible();
		await page.getByRole('button', { name: 'Retry playback', exact: true }).click();
		await vi.waitFor(() => expect(open).toHaveBeenCalledTimes(4));
	} finally {
		dispose();
	}
});

it('discards a scrub cursor that finishes opening after Close', async () => {
	const { next, open, mount, dispose } = keepFixture();
	const newer = cursor('newer');
	const pending = Promise.withResolvers<typeof next.playback>();
	try {
		await mount();
		await page.getByRole('button', { name: 'Close playback', exact: true }).click();
		open.mockReturnValueOnce(pending.promise).mockResolvedValueOnce(newer.playback);
		const finish = beginDrag();
		finish();
		await vi.waitFor(() => expect(open).toHaveBeenCalledTimes(2));
		await page.getByRole('button', { name: 'Close playback', exact: true }).click();
		await page.getByRole('button', { name: 'Refresh recordings', exact: true }).click();
		await vi.waitFor(() => expect(open).toHaveBeenCalledTimes(3));
		pending.resolve(next.playback);
		await vi.waitFor(() => expect(next.playback.close).toHaveBeenCalledOnce());
		expect(newer.playback.commitPlayback).not.toHaveBeenCalled();
		next.emitArchive(true);
		next.emitError('No stale scrub error should be shown.');
		await expect
			.element(page.getByRole('region', { name: 'Archive playback', exact: true }))
			.not.toBeInTheDocument();
		await expect.element(page.getByText(/No stale scrub error/)).not.toBeInTheDocument();
	} finally {
		pending.resolve(next.playback);
		newer.dispose();
		dispose();
	}
});
