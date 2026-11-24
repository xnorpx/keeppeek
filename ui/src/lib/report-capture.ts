const maxRecordingBytes = 100 * 1024 * 1024;
export const reportLimitMs = 5 * 60_000;
const recordingTypes = ['video/webm;codecs=vp8', 'video/webm', 'video/mp4'];

type EndReason =
	'stopped' | 'display-ended' | 'duration-limit' | 'byte-limit' | 'cancelled' | 'error';
type CaptureResult = { reason: EndReason; recording: Blob | null };

type CaptureOptions = {
	signal: AbortSignal;
	onSize?: (bytes: number) => void;
};

export function reportCaptureSupported(): boolean {
	return (
		typeof navigator !== 'undefined' &&
		typeof navigator.mediaDevices?.getDisplayMedia === 'function' &&
		typeof MediaRecorder !== 'undefined' &&
		recordingTypes.some((type) => MediaRecorder.isTypeSupported(type))
	);
}

/** Start from a user gesture. Rejected permission leaves the caller able to report with logs. */
export async function startReportCapture({ signal, onSize }: CaptureOptions) {
	signal.throwIfAborted();
	if (!reportCaptureSupported()) throw new Error('Screen recording is unavailable.');
	const mimeType = recordingTypes.find((type) => MediaRecorder.isTypeSupported(type));
	const stream = await navigator.mediaDevices.getDisplayMedia({ video: true, audio: false });
	const tracks = stream.getTracks();
	const release = () => tracks.forEach((track) => track.stop());
	let recorder: MediaRecorder;
	try {
		signal.throwIfAborted();
		if (stream.getAudioTracks().length > 0 || stream.getVideoTracks().length !== 1) {
			throw new Error('Screen capture did not return one video-only track.');
		}
		recorder = new MediaRecorder(stream, { mimeType });
	} catch (error) {
		release();
		throw error;
	}

	const display = stream.getVideoTracks()[0];
	const chunks: Blob[] = [];
	let complete!: (value: CaptureResult) => void;
	const result = new Promise<CaptureResult>((resolve) => {
		complete = resolve;
	});
	let bytes = 0;
	let reason: EndReason | null = null;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let finished = false;

	function finalize() {
		if (finished) return;
		finished = true;
		clearTimeout(timer);
		display.removeEventListener('ended', onDisplayEnded);
		signal.removeEventListener('abort', onAbort);
		recorder.removeEventListener('dataavailable', onData);
		recorder.removeEventListener('error', onError);
		recorder.removeEventListener('stop', finalize);
		release();
		const outcome = reason ?? 'display-ended';
		const recording =
			bytes > 0 && outcome !== 'cancelled' && outcome !== 'error'
				? new Blob(chunks, { type: (mimeType ?? '').split(';')[0] })
				: null;
		chunks.length = 0;
		complete({ reason: outcome, recording });
	}

	function stop(next: EndReason): Promise<CaptureResult> {
		if (!finished && (reason === null || next === 'cancelled' || next === 'error')) {
			reason = next;
			if (recorder.state !== 'inactive') recorder.stop();
		}
		return result;
	}

	const onDisplayEnded = () => {
		void stop('display-ended');
	};
	const onAbort = () => {
		void stop('cancelled');
		// Cancellation discards the tail, so page suspension must not delay releasing the display.
		finalize();
	};
	const onError = () => {
		void stop('error');
		finalize();
	};
	const onData = (event: BlobEvent) => {
		if (
			finished ||
			reason === 'cancelled' ||
			reason === 'error' ||
			reason === 'byte-limit' ||
			event.data.size === 0
		)
			return;
		if (bytes + event.data.size > maxRecordingBytes) {
			reason = 'byte-limit';
			if (recorder.state !== 'inactive') recorder.stop();
			return;
		}
		chunks.push(event.data);
		bytes += event.data.size;
		onSize?.(bytes);
		if (bytes === maxRecordingBytes && recorder.state !== 'inactive') void stop('byte-limit');
	};

	display.addEventListener('ended', onDisplayEnded);
	signal.addEventListener('abort', onAbort, { once: true });
	recorder.addEventListener('dataavailable', onData);
	recorder.addEventListener('error', onError);
	recorder.addEventListener('stop', finalize);
	try {
		recorder.start(1_000);
		timer = setTimeout(() => {
			void stop('duration-limit');
		}, reportLimitMs);
		if (display.readyState === 'ended') onDisplayEnded();
	} catch (error) {
		reason = 'error';
		finalize();
		throw error;
	}
	return { result, stop: () => stop('stopped') };
}
