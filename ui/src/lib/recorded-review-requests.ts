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

export function unsupportedCodecStatus(rejected: readonly RejectedRecordedStream[]): string {
	const codecs = rejected
		.map((candidate) => `${candidate.stream === 'main' ? 'Main' : 'Sub'} (${candidate.encoding})`)
		.join(', ');
	return `This browser cannot decode the available recordings: ${codecs}.`;
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
	const offset =
		options.appliedOffsetMs && options.appliedOffsetMs !== 0
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
