import { page, userEvent } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { create } from '@bufbuild/protobuf';
import { RecordedAudioTimingSchema, type RecordedAudioTiming } from '$lib/proto/webrtc_pb';
import { defaultPlaybackPreferences } from '$lib/playback-preferences';
import type { StoredMediaPlayback } from '$lib/control-client';
import type { CameraListItem, ProfileSummary } from '$lib/types';
import RecordedReviewWorkspace from './RecordedReviewWorkspace.svelte';

vi.mock('$app/paths', () => ({ resolve: (path: string) => path }));

type MockCursor = Pick<
	StoredMediaPlayback,
	| 'url'
	| 'anchorTimeMs'
	| 'canStepExactly'
	| 'setPlaying'
	| 'setPlaybackRate'
	| 'observe'
	| 'close'
	| 'onError'
	| 'onRecordedAudio'
	| 'onFrameClockUpdate'
	| 'storedNsForMediaUs'
	| 'mediaUsForStoredNs'
	| 'stepRecordedFrame'
> & {
	sourceId: string;
	streamId: 'main' | 'sub';
	emitAudio: (timing: RecordedAudioTiming) => void;
};

const clientState = vi.hoisted(() => ({
	ranges: [] as Array<{
		sourceId: string;
		streamId: 'main' | 'sub';
		startMs: number;
		endMs: number;
	}>,
	events: [] as Array<{ startMs: number }>,
	eventsTruncated: false,
	opened: [] as MockCursor[],
	openCalls: [] as Array<{ sourceId: string; streamId: string; timestampMs: number }>,
	stepResultNs: 1_700_000_010_040_000_000n,
	audioBySource: {} as Record<string, RecordedAudioTiming | undefined>,
	stepSupported: false
}));

function makeCursor(sourceId: string, streamId: 'main' | 'sub', timestampMs: number): MockCursor {
	let audioListener: ((timing: RecordedAudioTiming) => void) | undefined;
	const cursor: MockCursor = {
		sourceId,
		streamId,
		url: `blob:https://keeppeek.invalid/${sourceId}-${streamId}-${timestampMs}`,
		anchorTimeMs: timestampMs,
		canStepExactly: clientState.stepSupported,
		setPlaying: vi.fn(),
		setPlaybackRate: vi.fn(),
		observe: vi.fn(),
		close: vi.fn(async () => undefined),
		onError: () => () => undefined,
		onRecordedAudio: (listener) => {
			audioListener = listener;
			const preset = clientState.audioBySource[sourceId];
			if (preset) listener(preset);
			return () => {
				audioListener = undefined;
			};
		},
		onFrameClockUpdate: () => () => undefined,
		storedNsForMediaUs: (mediaUs: number) =>
			BigInt(timestampMs) * 1_000_000n + BigInt(mediaUs) * 1_000n,
		mediaUsForStoredNs: (storedNs: bigint) =>
			Number((storedNs - BigInt(timestampMs) * 1_000_000n) / 1_000n),
		stepRecordedFrame: vi.fn<StoredMediaPlayback['stepRecordedFrame']>(async () => {
			const canvas = document.createElement('canvas');
			canvas.width = 16;
			canvas.height = 16;
			const frame = new VideoFrame(canvas, { timestamp: 40_000 });
			return {
				frame,
				sampleToken: new Uint8Array([1, 2, 3]),
				storedTimeNs: clientState.stepResultNs,
				presentationTimeNs: clientState.stepResultNs,
				resumeTimesNs: [clientState.stepResultNs],
				sourceTicks: 3_600n,
				sourceTimescale: 90_000
			};
		}),
		emitAudio: (timing) => audioListener?.(timing)
	};
	return cursor;
}

vi.mock('$lib/control-context', () => ({
	useControlClient: () => ({
		onAccessState: () => () => undefined,
		queryStoredTimeline: async () => ({ ranges: clientState.ranges }),
		searchEventMetadata: async (options: { streamId: string; pageToken?: string }) => ({
			hits:
				options.streamId === 'main'
					? clientState.events.map((event, index) => ({
							eventId: `event-${options.pageToken ?? 'p1'}-${index}`,
							sourceId: 'cam-1',
							startMs: event.startMs
						}))
					: [],
			nextPageToken:
				options.streamId === 'main' && clientState.eventsTruncated
					? `next-${options.pageToken ?? '1'}`
					: '',
			candidatesTruncated: false
		}),
		openStoredMedia: async (options: {
			sourceId: string;
			streamId: 'main' | 'sub';
			timestampMs: number;
		}) => {
			clientState.openCalls.push({
				sourceId: options.sourceId,
				streamId: options.streamId,
				timestampMs: options.timestampMs
			});
			const cursor = makeCursor(options.sourceId, options.streamId, options.timestampMs);
			clientState.opened.push(cursor);
			return cursor;
		}
	})
}));

function profile(stream: 'main' | 'sub', encoding: string): ProfileSummary {
	return {
		name: `${stream}Stream`,
		stream,
		encoding,
		resolution: stream === 'main' ? '1920x1080' : '640x360',
		framerate: 25
	};
}

function camera(
	id: string,
	name: string,
	profiles: ProfileSummary[],
	groupIds?: string[]
): CameraListItem {
	return {
		id,
		ip: id,
		name,
		manufacturer: null,
		model: null,
		firmware_version: null,
		is_reolink: false,
		...(groupIds ? { group_ids: groupIds } : {}),
		profiles
	};
}

beforeEach(() => {
	clientState.ranges = [];
	clientState.events = [];
	clientState.eventsTruncated = false;
	clientState.opened = [];
	clientState.openCalls = [];
	clientState.audioBySource = {};
	clientState.stepSupported = false;
	Object.defineProperty(Navigator.prototype, 'hardwareConcurrency', {
		configurable: true,
		get: () => 8
	});
});

class AvcOnlyMediaSource {
	static isTypeSupported(contentType: string): boolean {
		return contentType.includes('avc1');
	}
}

describe('RecordedReviewWorkspace', () => {
	it('reports codec-specific unsupported status, fallback variant, audio state, and diagnostics', async () => {
		const originalMediaSource = globalThis.MediaSource;
		Object.assign(globalThis, { MediaSource: AvcOnlyMediaSource });
		const startMs = 1_700_000_010_000;
		clientState.ranges = [
			{ sourceId: 'cam-1', streamId: 'main', startMs: startMs - 5_000, endMs: startMs + 20_000 },
			{ sourceId: 'cam-1', streamId: 'sub', startMs: startMs - 5_000, endMs: startMs + 20_000 },
			{ sourceId: 'cam-2', streamId: 'main', startMs: startMs - 5_000, endMs: startMs + 20_000 }
		];
		clientState.audioBySource['cam-1'] = create(RecordedAudioTimingSchema, {
			configuredOffsetMs: 200,
			appliedOffsetMs: 200,
			audioPresent: true
		});
		const cameras = [
			camera('cam-1', 'Front Door', [profile('main', 'hevc'), profile('sub', 'h264')]),
			camera('cam-2', 'Driveway', [profile('main', 'hevc')])
		];
		try {
			await render(RecordedReviewWorkspace, {
				props: {
					cameras,
					selection: { cameraIds: ['cam-1', 'cam-2'], audioCameraId: 'cam-1' },
					startMs,
					focusedId: 'cam-1',
					preferences: {
						...defaultPlaybackPreferences(),
						media: { muted: false, playbackRate: 1, playing: false }
					},
					returnHref: '/events?date=2023-11-14',
					updateLink: vi.fn(),
					openExport: vi.fn(),
					single: vi.fn()
				}
			});

			await expect.element(page.getByRole('link', { name: 'Back to event' })).toBeVisible();
			await expect
				.element(
					page.getByText('This browser cannot decode the available recordings: Main (HEVC).')
				)
				.toBeVisible();
			await expect
				.element(page.getByText('No playable footage at this time'))
				.not.toBeInTheDocument();
			await expect
				.poll(() => document.querySelector('[data-review-variant]')?.textContent?.trim())
				.toBe('Sub · H264');
			await expect
				.poll(() => document.querySelector('[data-review-fallback]')?.textContent?.trim())
				.toBe('Playing Sub (Main uses HEVC)');
			await expect
				.poll(() => document.querySelector('[data-review-audio-state]')?.textContent?.trim())
				.toBe('Audio active (+200 ms)');

			await userEvent.click(page.getByText('Diagnostics'));
			await expect
				.poll(() => document.querySelector('[data-review-diagnostics]')?.textContent ?? '')
				.toContain('Corrections: 0');
		} finally {
			Object.assign(globalThis, { MediaSource: originalMediaSource });
		}
	});

	it('applies bounded group presets, jumps to events, and announces truncated UTC-day results', async () => {
		const startMs = 1_700_000_010_000;
		const updateLink = vi.fn();
		clientState.events = [{ startMs: startMs - 4_000 }, { startMs: startMs + 7_000 }];
		clientState.eventsTruncated = true;
		const cameras = [
			camera('cam-1', 'Cam 1', [profile('sub', 'h264')], ['Exterior']),
			camera('cam-2', 'Cam 2', [profile('sub', 'h264')], ['Exterior']),
			camera('cam-3', 'Cam 3', [profile('sub', 'h264')], ['Exterior']),
			camera('cam-4', 'Cam 4', [profile('sub', 'h264')], ['Exterior']),
			camera('cam-5', 'Cam 5', [profile('sub', 'h264')], ['Exterior'])
		];
		await render(RecordedReviewWorkspace, {
			props: {
				cameras,
				selection: { cameraIds: ['cam-1', 'cam-2'], audioCameraId: 'cam-1' },
				startMs,
				focusedId: 'cam-1',
				preferences: defaultPlaybackPreferences(),
				updateLink,
				openExport: vi.fn(),
				single: vi.fn()
			}
		});

		await expect
			.element(page.getByText('Event jumps use the first 2 events of this UTC day.'))
			.toBeVisible();

		const groupSelect = page.getByRole('combobox', { name: 'Camera group' });
		await expect.element(groupSelect).toBeVisible();
		await userEvent.selectOptions(groupSelect, 'Exterior');
		await expect.element(page.getByText('Showing 4 of 5 cameras in Exterior.')).toBeVisible();
		expect(updateLink).toHaveBeenLastCalledWith(
			expect.any(Number),
			'cam-1',
			expect.objectContaining({ cameraIds: ['cam-1', 'cam-2', 'cam-3', 'cam-4'] })
		);

		const nextEvent = page.getByRole('button', { name: 'Next event' });
		await expect.element(nextEvent).toBeEnabled();
		await userEvent.click(nextEvent);
		expect(updateLink).toHaveBeenLastCalledWith(startMs + 7_000, 'cam-1', expect.any(Object));

		const previousEvent = page.getByRole('button', { name: 'Previous event' });
		await expect.element(previousEvent).toBeEnabled();
		await userEvent.click(previousEvent);
		expect(updateLink).toHaveBeenLastCalledWith(startMs - 4_000, 'cam-1', expect.any(Object));
	});

	it('steps the focused paused camera and synchronizes sibling tiles to the stepped timestamp', async () => {
		const startMs = 1_700_000_010_000;
		const steppedMs = startMs + 40;
		clientState.stepSupported = true;
		clientState.stepResultNs = BigInt(steppedMs) * 1_000_000n;
		clientState.ranges = [
			{ sourceId: 'cam-1', streamId: 'sub', startMs: startMs - 5_000, endMs: startMs + 20_000 },
			{ sourceId: 'cam-2', streamId: 'sub', startMs: startMs - 5_000, endMs: startMs + 20_000 }
		];
		const updateLink = vi.fn();
		const cameras = [
			camera('cam-1', 'Cam 1', [profile('sub', 'h264')]),
			camera('cam-2', 'Cam 2', [profile('sub', 'h264')])
		];
		await render(RecordedReviewWorkspace, {
			props: {
				cameras,
				selection: { cameraIds: ['cam-1', 'cam-2'], audioCameraId: 'cam-1' },
				startMs,
				focusedId: 'cam-1',
				preferences: defaultPlaybackPreferences(),
				updateLink,
				openExport: vi.fn(),
				single: vi.fn()
			}
		});

		await expect.poll(() => clientState.opened.length).toBe(2);
		const videos = Array.from(document.querySelectorAll('video'));
		expect(videos).toHaveLength(2);
		const focusedVideo = videos[0]!;
		Object.defineProperty(focusedVideo, 'readyState', { configurable: true, get: () => 2 });
		Object.defineProperty(focusedVideo, 'currentSrc', {
			configurable: true,
			get: () => clientState.opened[0]!.url
		});
		focusedVideo.dispatchEvent(new Event('loadeddata'));
		await expect.poll(() => clientState.opened[0] !== undefined).toBe(true);
		Object.defineProperty(focusedVideo, 'currentTime', { configurable: true, get: () => 0 });
		const nextFrame = page.getByRole('button', { name: 'Next recorded frame' });
		// Provide a real VideoFrame-compatible paused anchor by mocking VideoFrame on the video element.
		const NativeVideoFrame = globalThis.VideoFrame;
		const FakeVideoFrame = function (source: CanvasImageSource, init?: VideoFrameInit) {
			if (source instanceof HTMLVideoElement) {
				const canvas = document.createElement('canvas');
				canvas.width = 16;
				canvas.height = 16;
				return new NativeVideoFrame(canvas, { timestamp: 0 });
			}
			return new NativeVideoFrame(source, init);
		} as unknown as typeof VideoFrame;
		Object.assign(globalThis, { VideoFrame: FakeVideoFrame });
		try {
			focusedVideo.dispatchEvent(new Event('loadeddata'));
			await expect.element(nextFrame).toBeVisible();
			const opensBeforeStep = clientState.openCalls.length;
			await userEvent.click(nextFrame);
			await expect.poll(() => updateLink.mock.calls.at(-1)?.[0]).toBe(steppedMs);
			// Only the non-focused camera reopens at the stepped timestamp; the focused tile keeps its canvas.
			await expect.poll(() => clientState.openCalls.length).toBe(opensBeforeStep + 1);
			expect(clientState.openCalls.at(-1)).toEqual({
				sourceId: 'cam-2',
				streamId: 'sub',
				timestampMs: steppedMs
			});
			const canvas = document.querySelector(
				'canvas[aria-label="Selected recorded picture for Cam 1"]'
			);
			expect(canvas?.hasAttribute('hidden')).toBe(false);
			// Changing focus after a frame step must not reopen either camera cursor.
			const opensAfterStep = clientState.openCalls.length;
			await userEvent.selectOptions(
				page.getByRole('combobox', { name: 'Focused camera' }),
				'cam-2'
			);
			await expect.poll(() => updateLink.mock.calls.at(-1)?.[1]).toBe('cam-2');
			expect(clientState.openCalls.length).toBe(opensAfterStep);
		} finally {
			Object.assign(globalThis, { VideoFrame: NativeVideoFrame });
		}
	});
});
