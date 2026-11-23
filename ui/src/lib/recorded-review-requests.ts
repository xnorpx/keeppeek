import { maximumReviewCameras } from './grid-playback-session';

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
