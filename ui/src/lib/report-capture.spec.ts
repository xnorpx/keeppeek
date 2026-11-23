import { afterEach, expect, it } from 'vitest';
import { startReportCapture } from './report-capture';

class Track extends EventTarget {
	readyState: MediaStreamTrackState = 'live';
	stopped = 0;
	stop() {
		this.stopped += 1;
		this.readyState = 'ended';
	}
}

class Recorder extends EventTarget {
	static latest: Recorder;
	static isTypeSupported = () => true;
	state: RecordingState = 'inactive';
	stopped = 0;
	constructor() {
		super();
		Recorder.latest = this;
	}
	start() {
		this.state = 'recording';
	}
	stop() {
		this.state = 'inactive';
		this.stopped += 1;
		queueMicrotask(() => {
			this.chunk(new Blob(['tail']));
			this.dispatchEvent(new Event('stop'));
		});
	}
	chunk(data: Blob) {
		this.dispatchEvent(Object.assign(new Event('dataavailable'), { data }));
	}
}

function stubGlobal(key: 'MediaRecorder' | 'navigator', value: unknown) {
	const original = Object.getOwnPropertyDescriptor(globalThis, key);
	Object.defineProperty(globalThis, key, { configurable: true, value });
	restore.push(() => {
		if (original) Object.defineProperty(globalThis, key, original);
		else Reflect.deleteProperty(globalThis, key);
	});
}

const restore: Array<() => void> = [];
afterEach(() => {
	while (restore.length) restore.pop()?.();
});

function installCapture(audio = false) {
	const display = new Track();
	const microphone = new Track();
	const tracks = audio ? [display, microphone] : [display];
	const stream = {
		getTracks: () => tracks,
		getVideoTracks: () => [display],
		getAudioTracks: () => (audio ? [microphone] : [])
	};
	const options: DisplayMediaStreamOptions[] = [];
	stubGlobal('MediaRecorder', Recorder);
	stubGlobal('navigator', {
		mediaDevices: {
			getDisplayMedia: async (capture: DisplayMediaStreamOptions) => {
				options.push(capture);
				return stream;
			}
		}
	});
	return { options, display, microphone, stream };
}

it('asks only for a display track and includes the final recorder chunk', async () => {
	const { options, display } = installCapture();
	const sizes: number[] = [];
	const controller = new AbortController();
	const capture = await startReportCapture({
		signal: controller.signal,
		onSize: (size) => sizes.push(size)
	});
	expect(options).toEqual([{ video: true, audio: false }]);
	Recorder.latest.chunk(new Blob(['head']));
	display.dispatchEvent(new Event('ended'));
	const result = await capture.stop();
	expect(result.reason).toBe('display-ended');
	expect(await result.recording?.text()).toBe('headtail');
	expect(sizes).toEqual([4, 8]);
	expect(Recorder.latest.stopped).toBe(1);
	expect(display.stopped).toBe(1);
});

it('discards cancelled output even when finalization has begun', async () => {
	const { display } = installCapture();
	const controller = new AbortController();
	const capture = await startReportCapture({ signal: controller.signal });
	Recorder.latest.chunk(new Blob(['private frame']));
	void capture.stop();
	controller.abort();
	expect(await capture.result).toEqual({ reason: 'cancelled', recording: null });
	expect(Recorder.latest.stopped).toBe(1);
	expect(display.stopped).toBe(1);
});

it('rejects an unexpected audio track and releases every track', async () => {
	const { display, microphone } = installCapture(true);
	await expect(startReportCapture({ signal: new AbortController().signal })).rejects.toThrow(
		'video-only'
	);
	expect(display.stopped).toBe(1);
	expect(microphone.stopped).toBe(1);
});

it('enforces the byte limit without retaining an oversized chunk', async () => {
	class OversizedChunk extends Blob {
		override get size() {
			return 100 * 1024 * 1024;
		}
	}
	const { display } = installCapture();
	const capture = await startReportCapture({ signal: new AbortController().signal });
	Recorder.latest.chunk(new Blob(['head']));
	Recorder.latest.chunk(new OversizedChunk());
	const result = await capture.result;
	expect(result.reason).toBe('byte-limit');
	expect(await result.recording?.text()).toBe('head');
	expect(Recorder.latest.stopped).toBe(1);
	expect(display.stopped).toBe(1);
});

it('releases a late display picker result after cancellation', async () => {
	const { display, stream } = installCapture();
	const picker = Promise.withResolvers<typeof stream>();
	stubGlobal('navigator', { mediaDevices: { getDisplayMedia: () => picker.promise } });
	const controller = new AbortController();
	const pending = startReportCapture({ signal: controller.signal });
	const rejection = pending.catch((error: unknown) => error);
	controller.abort();
	picker.resolve(stream);
	expect(await rejection).toMatchObject({ name: 'AbortError' });
	expect(display.stopped).toBe(1);
});
