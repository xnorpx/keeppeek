import type { ControlClient } from './control-client';
import type { KeepReviewRange } from './keep-link';
import type { RecordingSegment } from './types';
import { maximumReviewCameras } from './grid-playback-session';

// A control session accepts one background recording operation at a time.
// Each admitted camera can have an open, playback change or refill in flight.
export class RecordedReviewRequests {
	#tail = Promise.resolve();
	#pending = 0;

	run<T>(operation: () => Promise<T>, signal: AbortSignal): Promise<T> {
		if (this.#pending >= maximumReviewCameras * 3) {
			return Promise.reject(new Error('Recording comparison is busy.'));
		}
		this.#pending++;
		const expiresAt = performance.now() + 4000;
		const result = this.#tail.then(() => {
			signal.throwIfAborted();
			if (performance.now() >= expiresAt) throw new Error('Recording comparison timed out.');
			return operation();
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
