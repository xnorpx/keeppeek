import {
	StoredFrameStepStatus,
	type StoredFrameStepChunk,
	type StoredFrameStepResult
} from './proto/webrtc_pb';

const maxSamples = 4_096;
const maxBytes = 32 * 1_024 * 1_024;
const chunkBytes = 32 * 1_024;

type Sample = { rank: number; randomAccess: boolean; bytes: Uint8Array<ArrayBuffer> };

export type PresentedStoredFrame = {
	frame: VideoFrame;
	sampleToken: Uint8Array;
	presentationTimeNs: bigint;
	storedTimeNs: bigint;
	sourceTicks: bigint;
	sourceTimescale: number;
};

function nanoseconds(value: { seconds: bigint; nanos: number } | undefined): bigint {
	if (!value || value.nanos < 0 || value.nanos >= 1_000_000_000) {
		throw new Error('The recorder returned an invalid picture timestamp.');
	}
	return value.seconds * 1_000_000_000n + BigInt(value.nanos);
}

// A transfer belongs to one cursor generation and is replaced on seek, close or timeout.
export class StoredFrameWindow {
	readonly samples: Sample[] = [];
	#ranks = new Set<number>();
	#partial?: { sample: Sample; next: number };
	#allocated = 0;
	#result?: StoredFrameStepResult;
	#ready = Promise.withResolvers<void>();
	#error?: Error;

	constructor(
		readonly cursorId: string,
		readonly generation: bigint,
		readonly stepId: string
	) {}

	receive(chunk: StoredFrameStepChunk): void {
		if (
			this.#error ||
			chunk.storedMediaId !== this.cursorId ||
			chunk.generation !== this.generation ||
			chunk.stepId !== this.stepId
		)
			return;
		try {
			this.append(chunk);
			this.checkComplete();
		} catch (cause) {
			this.cancel(cause instanceof Error ? cause : new Error('Invalid recorded frame data.'));
		}
	}

	private append(chunk: StoredFrameStepChunk): void {
		const count = Math.ceil(chunk.sampleBytes / chunkBytes);
		const expectedLength = Math.min(chunkBytes, chunk.sampleBytes - chunk.chunkIndex * chunkBytes);
		if (
			chunk.decodeIndex !== this.samples.length ||
			chunk.decodeIndex >= maxSamples ||
			chunk.presentationRank >= maxSamples ||
			chunk.sampleBytes === 0 ||
			chunk.sampleBytes > maxBytes ||
			chunk.chunkCount !== count ||
			chunk.chunkIndex >= count ||
			chunk.payload.length !== expectedLength ||
			chunk.chunkIndex !== (this.#partial?.next ?? 0)
		)
			throw new Error('The recorded frame chunks are inconsistent.');
		if (!this.#partial) {
			if (
				this.#ranks.has(chunk.presentationRank) ||
				(chunk.decodeIndex === 0 && !chunk.randomAccess) ||
				this.#allocated + chunk.sampleBytes > (this.#result?.encodedBytes ?? maxBytes) ||
				(this.#result && chunk.decodeIndex >= this.#result.sampleCount)
			)
				throw new Error('The decode window exceeds its declared bounds.');
			this.#ranks.add(chunk.presentationRank);
			this.#allocated += chunk.sampleBytes;
			this.#partial = {
				sample: {
					bytes: new Uint8Array(chunk.sampleBytes),
					rank: chunk.presentationRank,
					randomAccess: chunk.randomAccess
				},
				next: 0
			};
		}
		const partial = this.#partial;
		if (
			partial.sample.bytes.length !== chunk.sampleBytes ||
			partial.sample.rank !== chunk.presentationRank ||
			partial.sample.randomAccess !== chunk.randomAccess
		)
			throw new Error('A recorded access unit changed during transfer.');
		partial.sample.bytes.set(chunk.payload, chunk.chunkIndex * chunkBytes);
		partial.next++;
		if (partial.next === count) {
			this.samples.push(partial.sample);
			this.#partial = undefined;
		}
	}

	private checkComplete(): void {
		const result = this.#result;
		if (!result) return;
		if (this.#allocated > result.encodedBytes || this.samples.length > result.sampleCount)
			throw new Error('Invalid frame window size.');
		if (this.samples.length < result.sampleCount) return;
		if (
			this.#partial ||
			this.#allocated !== result.encodedBytes ||
			this.samples.some((sample) => sample.rank >= result.sampleCount)
		)
			throw new Error('Incomplete recorded frame window.');
		this.#ready.resolve();
	}

	cancel(cause: Error): void {
		this.#error = cause;
		this.#partial = undefined;
		this.samples.length = 0;
		this.#ready.resolve();
	}

	async decode(result: StoredFrameStepResult, signal: AbortSignal): Promise<PresentedStoredFrame> {
		if (
			result.storedMediaId !== this.cursorId ||
			result.generation !== this.generation ||
			result.stepId !== this.stepId
		) {
			throw new Error('The recorded picture belongs to another cursor.');
		}
		if (result.status !== StoredFrameStepStatus.READY)
			throw new Error(stepUnavailable(result.status));
		const format = result.configuration?.format?.format;
		const codec = result.configuration?.codec?.name;
		if (
			!result.sampleToken.length ||
			result.sampleToken.length > 64 ||
			!result.sourceTimescale ||
			!result.sampleCount ||
			result.sampleCount > maxSamples ||
			!result.encodedBytes ||
			result.encodedBytes > maxBytes ||
			result.targetPresentationRank >= result.sampleCount ||
			!codec ||
			format?.case !== 'video'
		) {
			throw new Error('The recorder returned an invalid frame window.');
		}
		const presentationTimeNs = nanoseconds(result.presentationTime);
		const storedTimeNs = nanoseconds(result.storedTime);
		signal.throwIfAborted();
		const abort = () => this.cancel(new Error('Recorded frame operation cancelled.'));
		signal.addEventListener('abort', abort, { once: true });
		try {
			this.#result = result;
			this.checkComplete();
			await this.#ready.promise;
			if (this.#error) throw this.#error;
			const frame = await decodePicture(
				{
					codec,
					codedWidth: format.value.width,
					codedHeight: format.value.height,
					description: Uint8Array.from(format.value.decoderConfig)
				},
				this.samples,
				result.targetPresentationRank,
				signal
			);
			return {
				frame,
				sampleToken: result.sampleToken,
				presentationTimeNs,
				storedTimeNs,
				sourceTicks: result.sourceTicks,
				sourceTimescale: result.sourceTimescale
			};
		} finally {
			signal.removeEventListener('abort', abort);
			this.#partial = undefined;
			this.samples.length = 0;
		}
	}
}

function stepUnavailable(status: StoredFrameStepStatus): string {
	switch (status) {
		case StoredFrameStepStatus.AMBIGUOUS_ANCHOR:
			return 'More than one picture has this time. An exact starting picture is unavailable.';
		case StoredFrameStepStatus.BOUNDARY:
			return 'Reached the end of the available pictures in this direction.';
		case StoredFrameStepStatus.GAP:
			return 'The recording has a gap here.';
		case StoredFrameStepStatus.DISTANCE_LIMIT:
			return 'The adjacent picture exceeds the decode limit.';
		case StoredFrameStepStatus.STALE:
			return 'The recording changed. Select a picture again.';
		case StoredFrameStepStatus.DISCONTINUITY:
			return 'The recording clock or codec changed here.';
		default:
			return 'Original picture timing is unavailable for this recording.';
	}
}

async function decodePicture(
	config: VideoDecoderConfig,
	samples: Sample[],
	target: number,
	signal: AbortSignal
): Promise<VideoFrame> {
	if (
		typeof VideoDecoder === 'undefined' ||
		!(await VideoDecoder.isConfigSupported(config)).supported
	) {
		throw new Error('This browser cannot decode exact pictures for this stream.');
	}
	signal.throwIfAborted();
	let selected: VideoFrame | undefined;
	let failure: Error | undefined;
	const decoder = new VideoDecoder({
		output(frame) {
			if (frame.timestamp === target && !selected && !signal.aborted) selected = frame;
			else {
				if (frame.timestamp === target && selected)
					failure = new Error('The decoder returned an ambiguous picture.');
				frame.close();
			}
		},
		error(cause) {
			failure = cause;
		}
	});
	const close = () => {
		if (decoder.state !== 'closed') decoder.close();
	};
	signal.addEventListener('abort', close, { once: true });
	try {
		decoder.configure(config);
		for (const sample of samples) {
			decoder.decode(
				new EncodedVideoChunk({
					type: sample.randomAccess ? 'key' : 'delta',
					timestamp: sample.rank,
					data: sample.bytes
				})
			);
		}
		await decoder.flush();
		signal.throwIfAborted();
		if (failure) throw failure;
		if (!selected) throw new Error('The decoder did not return the requested picture.');
		const output = selected;
		selected = undefined;
		return output;
	} finally {
		close();
		selected?.close();
		signal.removeEventListener('abort', close);
	}
}
