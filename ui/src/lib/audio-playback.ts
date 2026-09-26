import type { AudioDataFrame } from './proto/webrtc_pb';

type BufferedAudioFrame = {
	frame: AudioDataFrame;
	arrivalMs: number;
};

const targetDelayMs = 80;
const maxFrames = 32;
const maxAgeMs = 500;

export class AudioJitterBuffer {
	#frames: BufferedAudioFrame[] = [];
	#timer: ReturnType<typeof setTimeout> | null = null;
	#startedAtMs: number | null = null;

	push(frame: AudioDataFrame, consume: (frame: AudioDataFrame) => void): void {
		const now = performance.now();
		this.#frames = this.#frames.filter((item) => now - item.arrivalMs <= maxAgeMs);
		if (this.#frames.some((item) => item.frame.frameId === frame.frameId)) return;
		this.#frames.push({ frame, arrivalMs: now });
		this.#frames.sort((left, right) => Number(left.frame.frameId - right.frame.frameId));
		while (this.#frames.length > maxFrames) this.#frames.shift();
		this.#schedule(consume);
	}

	clear(): void {
		if (this.#timer !== null) clearTimeout(this.#timer);
		this.#timer = null;
		this.#frames = [];
		this.#startedAtMs = null;
	}

	#schedule(consume: (frame: AudioDataFrame) => void): void {
		if (this.#timer !== null) return;
		const delay = this.#startedAtMs === null ? targetDelayMs : 0;
		this.#timer = setTimeout(() => {
			this.#timer = null;
			this.#startedAtMs ??= performance.now();
			const frame = this.#frames.shift();
			if (frame) consume(frame.frame);
			if (this.#frames.length > 0) this.#schedule(consume);
		}, delay);
	}
}

export class AacAudioPlayback {
	readonly #context = new AudioContext();
	readonly #jitter = new AudioJitterBuffer();
	#decoder: AudioDecoder | null = null;
	#nextStart = 0;
	#closed = false;

	async configure(
		codec: string,
		decoderConfig: Uint8Array,
		sampleRate: number,
		channelCount: number
	): Promise<void> {
		if (!('AudioDecoder' in globalThis)) {
			throw new Error('This browser does not support AAC WebCodecs playback.');
		}
		const config: AudioDecoderConfig = {
			codec: codec === 'aac' ? 'mp4a.40.2' : codec,
			sampleRate,
			numberOfChannels: channelCount,
			description: decoderConfig
		};
		const support = await AudioDecoder.isConfigSupported(config);
		if (!support.supported) throw new Error(`AAC configuration is unsupported: ${config.codec}`);
		this.#decoder = new AudioDecoder({
			output: (audioData) => this.#render(audioData),
			error: (error) => console.warn('AAC decoder stopped', error)
		});
		this.#decoder.configure(config);
	}

	async resume(): Promise<void> {
		if (this.#context.state === 'suspended') await this.#context.resume();
	}

	push(frame: AudioDataFrame): void {
		if (this.#closed || !this.#decoder) return;
		this.#jitter.push(frame, (ready) => {
			const timestampUs = ready.timestamp
				? Number(ready.timestamp.seconds) * 1_000_000 + ready.timestamp.nanos / 1_000
				: 0;
			const durationUs = ready.duration
				? Number(ready.duration.seconds) * 1_000_000 + ready.duration.nanos / 1_000
				: 64_000;
			this.#decoder?.decode(
				new EncodedAudioChunk({
					type: 'key',
					timestamp: timestampUs,
					duration: durationUs,
					data: ready.payload
				})
			);
		});
	}

	close(): void {
		this.#closed = true;
		this.#jitter.clear();
		if (this.#decoder?.state !== 'closed') this.#decoder?.close();
		void this.#context.close();
	}

	#render(audioData: AudioData): void {
		if (this.#closed) {
			audioData.close();
			return;
		}
		const buffer = this.#context.createBuffer(
			audioData.numberOfChannels,
			audioData.numberOfFrames,
			audioData.sampleRate
		);
		for (let channel = 0; channel < audioData.numberOfChannels; channel += 1) {
			audioData.copyTo(buffer.getChannelData(channel), {
				planeIndex: channel,
				format: 'f32-planar'
			});
		}
		audioData.close();
		const source = this.#context.createBufferSource();
		source.buffer = buffer;
		source.connect(this.#context.destination);
		this.#nextStart = Math.max(this.#nextStart, this.#context.currentTime);
		source.start(this.#nextStart);
		this.#nextStart += buffer.duration;
	}
}
