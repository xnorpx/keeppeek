import { describe, expect, it } from 'vitest';
import {
	adjacentReviewEvent,
	loadReviewDayEvents,
	maximumReviewEventPages,
	RecordedReviewRequests,
	reviewAudioStatusLabel,
	reviewExportRecordings,
	reviewGroupPresets,
	unsupportedCodecStatus
} from './recorded-review-requests';
import type { CameraListItem } from './types';

describe('comparison recorder admission', () => {
	it('serializes operations across cameras and skips a withdrawn camera', async () => {
		const lane = new RecordedReviewRequests();
		const current = new AbortController();
		const removed = new AbortController();
		const first = Promise.withResolvers<void>();
		const order: number[] = [];
		const initial = lane.run(async () => {
			order.push(1);
			await first.promise;
		}, current.signal);
		const withdrawn = lane.run(async () => order.push(2), removed.signal);
		const final = lane.run(async () => order.push(3), current.signal);
		removed.abort();
		await Promise.resolve();
		expect(order).toEqual([1]);
		first.resolve();
		await initial;
		await expect(withdrawn).rejects.toThrow();
		await final;
		expect(order).toEqual([1, 3]);
	});
	it('releases a stalled pre-dispatch operation and prevents a late network send', async () => {
		const lane = new RecordedReviewRequests(),
			signal = new AbortController().signal;
		const connection = Promise.withResolvers<void>();
		const completed = Promise.withResolvers<boolean>();
		const pending = lane.run(async (beforeSend) => {
			await connection.promise;
			try {
				beforeSend();
				completed.resolve(true);
			} catch {
				completed.resolve(false);
			}
		}, signal);
		await expect(pending).rejects.toThrow('timed out');
		await expect(
			lane.run(async (beforeSend) => {
				beforeSend();
				return 'healthy';
			}, signal)
		).resolves.toBe('healthy');
		connection.resolve();
		await expect(completed.promise).resolves.toBe(false);
	}, 10_000);
});

it('queries only the UTC days touched by the half-open marks and stops after cancellation', async () => {
	const boundary = Date.UTC(2026, 8, 6);
	const dates: string[] = [];
	const client = {
		getRecordingsInRange: async (_cameras: readonly string[], date: string) => {
			dates.push(date);
			return [];
		}
	};
	const controller = new AbortController();
	await reviewExportRecordings(
		client,
		'front',
		{ startMs: boundary - 1, endMs: boundary },
		controller.signal
	);
	expect(dates).toEqual(['2026-09-05']);
	dates.length = 0;
	await reviewExportRecordings(
		client,
		'front',
		{ startMs: boundary - 1, endMs: boundary + 1 },
		controller.signal
	);
	expect(dates).toEqual(['2026-09-05', '2026-09-06']);
	controller.abort();
	await expect(
		reviewExportRecordings(
			client,
			'front',
			{ startMs: boundary - 1, endMs: boundary + 1 },
			controller.signal
		)
	).rejects.toThrow();
	expect(dates).toHaveLength(2);
});

describe('synchronized review helpers', () => {
	function camera(id: string, groupIds?: string[]): CameraListItem {
		return {
			id,
			ip: id,
			name: id,
			manufacturer: null,
			model: null,
			firmware_version: null,
			is_reolink: false,
			group_ids: groupIds,
			profiles: []
		};
	}

	it('builds sorted bounded group presets with at least two authorized cameras', () => {
		const presets = reviewGroupPresets([
			camera('cam-1', ['Exterior', 'Solo', ' ']),
			camera('cam-2', ['Exterior', 'Garage']),
			camera('cam-3', ['Exterior', 'Garage']),
			camera('cam-4', ['Exterior']),
			camera('cam-5', ['Exterior'])
		]);
		expect(presets).toEqual([
			{
				name: 'Exterior',
				cameraIds: ['cam-1', 'cam-2', 'cam-3', 'cam-4'],
				totalCameras: 5
			},
			{
				name: 'Garage',
				cameraIds: ['cam-2', 'cam-3'],
				totalCameras: 2
			}
		]);
	});

	it('formats codec-specific unsupported recording messages', () => {
		expect(
			unsupportedCodecStatus([
				{ stream: 'main', encoding: 'hevc' },
				{ stream: 'sub', encoding: 'h265' }
			])
		).toBe('This browser cannot decode the available recordings: Main (HEVC), Sub (H265).');
	});

	it('paginates UTC-day events up to the page bound and flags truncation or total failure', async () => {
		let mainPages = 0;
		const client = {
			searchEventMetadata: async (options: { streamId: string; pageToken?: string }) => {
				if (options.streamId === 'main') {
					mainPages++;
					return {
						hits: [{ startMs: 1_000 + mainPages * 100 }],
						nextPageToken: `page-${mainPages + 1}`,
						candidatesTruncated: false
					};
				}
				return { hits: [], nextPageToken: '', candidatesTruncated: false };
			}
		};
		const bounded = await loadReviewDayEvents(
			client as unknown as Parameters<typeof loadReviewDayEvents>[0],
			['cam-1'],
			0,
			new AbortController().signal
		);
		expect(mainPages).toBe(maximumReviewEventPages);
		expect(bounded.truncated).toBe(true);
		expect(bounded.failed).toBe(false);
		expect(bounded.events).toHaveLength(maximumReviewEventPages);

		const failingClient = {
			searchEventMetadata: async () => {
				throw new Error('Event search unavailable');
			}
		};
		const failed = await loadReviewDayEvents(
			failingClient as unknown as Parameters<typeof loadReviewDayEvents>[0],
			['cam-1'],
			0,
			new AbortController().signal
		);
		expect(failed).toEqual({ events: [], truncated: false, failed: true });
	});

	it('finds nearest previous and next review events across cameras', () => {
		const events = [
			{ startMs: 1_000 },
			{ startMs: 5_000 },
			{ startMs: 5_000 },
			{ startMs: 9_000 },
			{ startMs: Number.NaN }
		];
		expect(adjacentReviewEvent(events, 5_000, -1)).toBe(1_000);
		expect(adjacentReviewEvent(events, 5_000, 1)).toBe(9_000);
		expect(adjacentReviewEvent(events, 1_000, -1)).toBeNull();
		expect(adjacentReviewEvent(events, 9_000, 1)).toBeNull();
	});

	it('describes active, muted, unavailable, and corrected-offset audio states', () => {
		expect(reviewAudioStatusLabel({ isAudioSource: true, muted: false })).toBe('Audio active');
		expect(
			reviewAudioStatusLabel({ isAudioSource: true, muted: false, appliedOffsetMs: 200 })
		).toBe('Audio active (+200 ms)');
		expect(
			reviewAudioStatusLabel({ isAudioSource: true, muted: true, appliedOffsetMs: -150 })
		).toBe('Audio muted (-150 ms)');
		expect(reviewAudioStatusLabel({ isAudioSource: true, muted: false, audioPresent: false })).toBe(
			'Audio unavailable'
		);
		expect(reviewAudioStatusLabel({ isAudioSource: false, muted: false, appliedOffsetMs: 0 })).toBe(
			'Muted'
		);
	});
});
