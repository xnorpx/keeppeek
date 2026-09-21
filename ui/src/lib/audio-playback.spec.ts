import { describe, expect, it, vi } from 'vitest';
import { AudioJitterBuffer } from './audio-playback';
import { create } from '@bufbuild/protobuf';
import { AudioDataFrameSchema } from './proto/webrtc_pb';

describe('AudioJitterBuffer', () => {
	it('holds the first frame for the target delay and emits frames in order', () => {
		vi.useFakeTimers();
		const buffer = new AudioJitterBuffer();
		const emitted: bigint[] = [];
		const frame = (frameId: bigint) =>
			create(AudioDataFrameSchema, { frameId, payload: new Uint8Array([1]) });

		buffer.push(frame(2n), (item) => emitted.push(item.frameId));
		buffer.push(frame(1n), (item) => emitted.push(item.frameId));
		expect(emitted).toEqual([]);
		vi.advanceTimersByTime(80);
		expect(emitted).toEqual([1n]);
		vi.advanceTimersByTime(1);
		expect(emitted).toEqual([1n, 2n]);
		buffer.clear();
		vi.useRealTimers();
	});
});
