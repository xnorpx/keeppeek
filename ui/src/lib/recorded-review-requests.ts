import type { ControlClient } from './control-client';
import type { KeepReviewRange } from './keep-link';
import type { CameraListItem, RecordingSegment } from './types';
import { maximumReviewCameras } from './grid-playback-session';
import { formatRecordedAudioOffset } from './recorded-audio';
import type { RejectedRecordedStream } from './recorded-playback-policy';

export type ReviewGroupPreset = {
	name: string;
	cameraIds: readonly string[];
	totalCameras: number;
};

export const maximumReviewEventPages = 4;
export const reviewEventPageSize = 64;

export type ReviewDayEventsResult = {
	events: readonly { startMs: number }[];
	truncated: boolean;
	failed: boolean;
};

export function reviewGroupPresets(cameras: readonly CameraListItem[]): ReviewGroupPreset[] {
	const groups = new Map<string, string[]>();
	for (const camera of cameras) {
		for (const raw of camera.group_ids ?? []) {
			const name = raw.trim();
			if (!name) continue;
			const list = groups.get(name) ?? [];
			if (!list.includes(camera.id)) list.push(camera.id);
			groups.set(name, list);
		}
	}
	return [...groups.entries()]
		.filter(([, ids]) => ids.length >= 2)
		.toSorted(([left], [right]) => left.localeCompare(right))
		.map(([name, ids]) => ({
			name,
			cameraIds: ids.slice(0, maximumReviewCameras),
			totalCameras: ids.length
		}));
}

export function formatCodecLabel(encoding: string): string {
	return encoding.trim().toUpperCase();
}

export function unsupportedCodecStatus(rejected: readonly RejectedRecordedStream[]): string {
	const codecs = rejected
		.map(
			(candidate) =>
				`${candidate.stream === 'main' ? 'Main' : 'Sub'} (${formatCodecLabel(candidate.encoding)})`
		)
		.join(', ');
	return `This browser cannot decode the available recordings: ${codecs}.`;
}

export async function loadReviewDayEvents(
	client: Pick<ControlClient, 'searchEventMetadata'>,
	sourceIds: readonly string[],
	dayStartMs: number,
	signal: AbortSignal
): Promise<ReviewDayEventsResult> {
	const streams = ['main', 'sub'] as const;
	const outcomes = await Promise.allSettled(
		streams.map(async (streamId) => {
			const startTimes: number[] = [];
			let pageToken: string | undefined;
			let truncated = false;
			for (let page = 0; page < maximumReviewEventPages; page++) {
				signal.throwIfAborted();
				const result = await client.searchEventMetadata({
					sourceIds,
					streamId,
					startMs: dayStartMs,
					endMs: dayStartMs + 86_400_000,
					pageSize: reviewEventPageSize,
					...(pageToken ? { pageToken } : {}),
					signal
				});
				for (const hit of result.hits) startTimes.push(hit.startMs);
				if (result.candidatesTruncated) truncated = true;
				if (!result.nextPageToken) break;
				if (page + 1 === maximumReviewEventPages) {
					truncated = true;
					break;
				}
				pageToken = result.nextPageToken;
			}
			return { startTimes, truncated };
		})
	);
	const times = new Set<number>();
	let truncated = false;
	let succeeded = 0;
	for (const outcome of outcomes) {
		if (outcome.status !== 'fulfilled') continue;
		succeeded++;
		if (outcome.value.truncated) truncated = true;
		for (const startMs of outcome.value.startTimes) times.add(startMs);
	}
	return {
		events: [...times].toSorted((left, right) => left - right).map((startMs) => ({ startMs })),
		truncated,
		failed: succeeded === 0
	};
}

export function adjacentReviewEvent(
	events: readonly { startMs: number }[],
	epochMs: number,
	direction: -1 | 1
): number | null {
	let best: number | null = null;
	for (const event of events) {
		if (!Number.isFinite(event.startMs)) continue;
		if (direction < 0 && event.startMs < epochMs && (best === null || event.startMs > best)) {
			best = event.startMs;
		}
		if (direction > 0 && event.startMs > epochMs && (best === null || event.startMs < best)) {
			best = event.startMs;
		}
	}
	return best;
}

export function reviewAudioStatusLabel(options: {
	isAudioSource: boolean;
	muted: boolean;
	audioPresent?: boolean | null;
	appliedOffsetMs?: number | null;
}): string {
	if (options.audioPresent === false) return 'Audio unavailable';
	const offset = options.appliedOffsetMs
		? ` (${formatRecordedAudioOffset(options.appliedOffsetMs)})`
		: '';
	if (!options.isAudioSource) return `Muted${offset}`;
	return `${options.muted ? 'Audio muted' : 'Audio active'}${offset}`;
}

// A control session accepts one background recording operation at a time.
// Each admitted camera can have an open, playback change or refill in flight.
export class RecordedReviewRequests {
	#tail = Promise.resolve();
	#pending = 0;

	run<T>(operation: (beforeSend: () => void) => Promise<T>, signal: AbortSignal): Promise<T> {
		if (this.#pending >= maximumReviewCameras * 3) {
			return Promise.reject(new Error('Recording comparison is busy.'));
		}
		this.#pending++;
		const expiresAt = performance.now() + 4000;
		const result = this.#tail.then(async () => {
			signal.throwIfAborted();
			const remaining = expiresAt - performance.now();
			const expired = () => new Error('Recording comparison timed out.');
			if (remaining <= 0) throw expired();
			let active = true;
			let timer: ReturnType<typeof setTimeout> | undefined;
			const waitingForSend = new Promise<never>((_, reject) => {
				timer = setTimeout(() => {
					active = false;
					reject(expired());
				}, remaining);
			});
			const beforeSend = () => {
				signal.throwIfAborted();
				if (!active || performance.now() >= expiresAt) throw expired();
				// The bounded control-response timer owns the lane after this synchronous admission.
				clearTimeout(timer);
			};
			try {
				return await Promise.race([operation(beforeSend), waitingForSend]);
			} finally {
				active = false;
				clearTimeout(timer);
			}
		});
		this.#tail = result
			.then(
				() => undefined,
				() => undefined
			)
			.finally(() => this.#pending--);
		return result;
	}
}

export async function reviewExportRecordings(
	client: Pick<ControlClient, 'getRecordingsInRange'>,
	source: string,
	range: KeepReviewRange,
	signal: AbortSignal
): Promise<RecordingSegment[]> {
	const { startMs, endMs } = range;
	const days = new Set([startMs, endMs - 1].map((ms) => new Date(ms).toISOString().slice(0, 10)));
	const rows: RecordingSegment[] = [];
	for (const day of days) {
		signal.throwIfAborted();
		const result = await client.getRecordingsInRange([source], day, startMs, endMs, signal);
		signal.throwIfAborted();
		const recording = result.find((row) => row.camera_id === source);
		if (recording) rows.push(...recording.segments);
	}
	return rows;
}
