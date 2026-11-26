export const MAX_TIMELAPSE_RANGE_MS = 31 * 86_400_000;
export const MAX_TIMELAPSE_SAMPLES = 3_600;
export const MIN_TIMELAPSE_OUTPUT_FPS = 1;
export const MAX_TIMELAPSE_OUTPUT_FPS = 30;
export const MAX_TIMELAPSE_OUTPUT_WIDTH = 1_920;
export const MAX_TIMELAPSE_OUTPUT_HEIGHT = 1_080;
export const MAX_CONCURRENT_THUMBNAIL_FETCHES = 2;
export const MAX_THUMBNAIL_CACHE_BYTES = 64 * 1024 * 1024;

export type TimelapsePreset = 'last_15m' | 'last_hour' | 'custom';

export type TimelapseGapReason =
	'missing' | 'retained' | 'privacy' | 'unsupported_codec' | 'corrupt' | 'open_recording';

export type TimelapsePlanSample = {
	kind: 'sample';
	index: number;
	requestedTimeMs: number;
	actualTimeMs: number;
	sourceTicks?: bigint;
	sourceTimescale?: number;
	recordingId: string;
	fragmentSequence: bigint;
	sampleOrdinal: number;
	codec: string;
	width: number;
	height: number;
	encodedBytes: number;
	sampleToken: Uint8Array;
	tokenKey: string;
};

export type TimelapsePlanGap = {
	kind: 'gap';
	startMs: number;
	endMs: number;
	reason: TimelapseGapReason;
};

export type TimelapsePlanEntry = TimelapsePlanSample | TimelapsePlanGap;

export type TimelapseEstimate = {
	sampleCount: number;
	gapCount: number;
	gapDurationMs: number;
	outputDurationMs: number;
	estimatedSourceBytes: number;
	estimatedOutputBytes: number;
	requiresConfirmation: boolean;
	encoderAvailable: boolean;
	siteTimezone: string;
};

export type TimelapsePlanResponse = {
	sourceId: string;
	streamId: 'main' | 'sub';
	requestedStartMs: number;
	requestedEndMs: number;
	sourceFingerprint: string;
	catalogRevision: bigint;
	estimate: TimelapseEstimate;
	items: TimelapsePlanEntry[];
	totalSamples: number;
	nextPageToken: string;
};

export type TimelapseDecodedWindow = {
	tokenKey: string;
	actualTimeMs: number;
	targetPresentationRank: number;
	codec: string;
	width: number;
	height: number;
	decoderConfig: Uint8Array;
	samples: Array<{
		decodeIndex: number;
		presentationRank: number;
		randomAccess: boolean;
		payload: Uint8Array;
	}>;
};

export type TimelapseExportDraftOptions = {
	cadence: { mode: 'interval'; intervalMs: number } | { mode: 'count'; targetSampleCount: number };
	toleranceMs?: number;
	outputFps: number;
	maxWidth: number;
	maxHeight: number;
	timestampOverlay: boolean;
	confirmedExpensive: boolean;
	expectedSourceFingerprint?: string;
};

export type TimelapseJobSummaryView = {
	sampleCount: number;
	gapCount: number;
	outputDurationMs: number;
	outputWidth: number;
	outputHeight: number;
	silent: boolean;
	sourceFingerprint: string;
	siteTimezone: string;
	manifestSchemaVersion: number;
	manifestFileName: string | null;
	manifestSha256: string | null;
	manifestBytes: number | null;
};

export function tokenHexKey(token: Uint8Array): string {
	return Array.from(token, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

export function resolveTimelapsePresetRange(
	preset: TimelapsePreset,
	nowMs: number,
	custom?: { startMs: number; endMs: number }
): { startMs: number; endMs: number } {
	if (preset === 'last_15m') {
		return { startMs: nowMs - 15 * 60_000, endMs: nowMs };
	}
	if (preset === 'last_hour') {
		return { startMs: nowMs - 60 * 60_000, endMs: nowMs };
	}
	const startMs = custom?.startMs ?? nowMs - 60 * 60_000;
	const endMs = custom?.endMs ?? nowMs;
	if (!Number.isFinite(startMs) || !Number.isFinite(endMs) || endMs <= startMs) {
		throw new Error('Custom timelapse end time must follow start time.');
	}
	if (endMs - startMs > MAX_TIMELAPSE_RANGE_MS) {
		throw new Error('Timelapse range cannot exceed 31 days.');
	}
	return { startMs, endMs };
}

export function timelapseGapLabel(reason: TimelapseGapReason): string {
	switch (reason) {
		case 'missing':
			return 'No recorded footage';
		case 'retained':
			return 'Removed by retention';
		case 'privacy':
			return 'Privacy active';
		case 'unsupported_codec':
			return 'Unsupported video codec';
		case 'corrupt':
			return 'Unreadable recording segment';
		case 'open_recording':
			return 'Recording still open';
	}
}

export function formatBytesCompact(bytes: number): string {
	if (bytes < 1024) return `${bytes} B`;
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
	if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
	return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
}

export function formatDurationCompact(ms: number): string {
	if (ms < 1000) return `${ms} ms`;
	const totalSeconds = Math.round(ms / 1000);
	const minutes = Math.floor(totalSeconds / 60);
	const seconds = totalSeconds % 60;
	if (minutes === 0) return `${seconds}s`;
	return `${minutes}m ${seconds}s`;
}

type CacheEntry = {
	url: string;
	bytes: number;
};

/** Bounded LRU object-URL cache for lazy timelapse thumbnails. */
export class TimelapseThumbnailCache {
	readonly #maxBytes: number;
	readonly #revoke: (url: string) => void;
	readonly #entries = new Map<string, CacheEntry>();
	#totalBytes = 0;

	constructor(
		maxBytes = MAX_THUMBNAIL_CACHE_BYTES,
		revoke: (url: string) => void = (url) => URL.revokeObjectURL(url)
	) {
		this.#maxBytes = Math.max(1, maxBytes);
		this.#revoke = revoke;
	}

	get totalBytes(): number {
		return this.#totalBytes;
	}

	get size(): number {
		return this.#entries.size;
	}

	get(key: string): string | null {
		const entry = this.#entries.get(key);
		if (!entry) return null;
		this.#entries.delete(key);
		this.#entries.set(key, entry);
		return entry.url;
	}

	set(key: string, url: string, bytes: number): void {
		const normalizedBytes = Math.max(1, bytes);
		const existing = this.#entries.get(key);
		if (existing) {
			this.#totalBytes -= existing.bytes;
			this.#revoke(existing.url);
			this.#entries.delete(key);
		}
		this.#entries.set(key, { url, bytes: normalizedBytes });
		this.#totalBytes += normalizedBytes;
		this.#evictOverflow();
	}

	clear(): void {
		for (const entry of this.#entries.values()) {
			this.#revoke(entry.url);
		}
		this.#entries.clear();
		this.#totalBytes = 0;
	}

	#evictOverflow(): void {
		while (this.#totalBytes > this.#maxBytes && this.#entries.size > 1) {
			const oldestKey = this.#entries.keys().next().value;
			if (oldestKey === undefined) break;
			const evicted = this.#entries.get(oldestKey);
			if (evicted) {
				this.#totalBytes -= evicted.bytes;
				this.#revoke(evicted.url);
			}
			this.#entries.delete(oldestKey);
		}
	}
}

export type ThumbnailLoader = (
	sample: TimelapsePlanSample,
	signal: AbortSignal
) => Promise<{ url: string; bytes: number }>;

/** Bounded-concurrency lazy loader that cancels in-flight work on camera or range changes. */
export class TimelapseThumbnailQueue {
	readonly #maxConcurrent: number;
	readonly #cache: TimelapseThumbnailCache;
	readonly #loader: ThumbnailLoader;
	readonly #onUpdate: (key: string, state: { url?: string; error?: string }) => void;
	#generation = 0;
	#inFlight = new Map<string, AbortController>();
	#pending: TimelapsePlanSample[] = [];
	#queuedKeys = new Set<string>();

	constructor(
		cache: TimelapseThumbnailCache,
		loader: ThumbnailLoader,
		onUpdate: (key: string, state: { url?: string; error?: string }) => void,
		maxConcurrent = MAX_CONCURRENT_THUMBNAIL_FETCHES
	) {
		this.#cache = cache;
		this.#loader = loader;
		this.#onUpdate = onUpdate;
		this.#maxConcurrent = Math.max(1, maxConcurrent);
	}

	get activeCount(): number {
		return this.#inFlight.size;
	}

	get pendingCount(): number {
		return this.#pending.length;
	}

	request(sample: TimelapsePlanSample): void {
		const cached = this.#cache.get(sample.tokenKey);
		if (cached) {
			this.#onUpdate(sample.tokenKey, { url: cached });
			return;
		}
		if (this.#inFlight.has(sample.tokenKey) || this.#queuedKeys.has(sample.tokenKey)) {
			return;
		}
		this.#queuedKeys.add(sample.tokenKey);
		this.#pending.push(sample);
		this.#pump();
	}

	reset(): void {
		this.#generation += 1;
		for (const controller of this.#inFlight.values()) {
			controller.abort();
		}
		this.#inFlight.clear();
		this.#pending = [];
		this.#queuedKeys.clear();
		this.#cache.clear();
	}

	#pump(): void {
		while (this.#inFlight.size < this.#maxConcurrent && this.#pending.length > 0) {
			const sample = this.#pending.shift()!;
			this.#queuedKeys.delete(sample.tokenKey);
			const generation = this.#generation;
			const controller = new AbortController();
			this.#inFlight.set(sample.tokenKey, controller);
			void this.#loader(sample, controller.signal)
				.then(
					({ url, bytes }) => {
						if (generation !== this.#generation || controller.signal.aborted) {
							try {
								URL.revokeObjectURL(url);
							} catch {
								// Non-blob URLs in unit tests need no revocation.
							}
							return;
						}
						this.#cache.set(sample.tokenKey, url, bytes);
						this.#onUpdate(sample.tokenKey, { url });
					},
					(cause: unknown) => {
						if (generation !== this.#generation || controller.signal.aborted) return;
						const message =
							cause instanceof Error ? cause.message : 'Unable to decode sample preview.';
						this.#onUpdate(sample.tokenKey, { error: message });
					}
				)
				.finally(() => {
					if (this.#inFlight.get(sample.tokenKey) === controller) {
						this.#inFlight.delete(sample.tokenKey);
					}
					if (generation === this.#generation) {
						this.#pump();
					}
				});
		}
	}
}

/** Decodes a bounded GOP sample window via WebCodecs and returns a JPEG Blob for the target picture. */
export async function decodeTimelapseWindowToBlob(
	windowData: TimelapseDecodedWindow,
	signal?: AbortSignal
): Promise<Blob> {
	signal?.throwIfAborted();
	if (typeof VideoDecoder === 'undefined') {
		throw new Error('Browser WebCodecs VideoDecoder is unavailable.');
	}
	const config: VideoDecoderConfig = {
		codec: windowData.codec,
		codedWidth: windowData.width,
		codedHeight: windowData.height,
		description: windowData.decoderConfig
	};
	const support = await VideoDecoder.isConfigSupported(config);
	if (!support.supported) {
		throw new Error(`Browser cannot decode ${windowData.codec} previews.`);
	}
	let matched: VideoFrame | null = null;
	let fallback: VideoFrame | null = null;
	let decodeError: Error | null = null;
	const decoder = new VideoDecoder({
		output(frame) {
			if (frame.timestamp === windowData.targetPresentationRank) {
				matched?.close();
				matched = frame;
			} else {
				fallback?.close();
				fallback = frame;
			}
		},
		error(error) {
			decodeError = error instanceof Error ? error : new Error('VideoDecoder failed.');
		}
	});
	try {
		decoder.configure(config);
		for (const unit of windowData.samples) {
			signal?.throwIfAborted();
			decoder.decode(
				new EncodedVideoChunk({
					type: unit.randomAccess ? 'key' : 'delta',
					timestamp: unit.presentationRank,
					data: unit.payload
				})
			);
		}
		await decoder.flush();
		if (decodeError) throw decodeError;
		const chosen = (matched ?? fallback) as VideoFrame | null;
		if (!chosen) {
			throw new Error('Decoder produced no picture for the sampled frame.');
		}
		return await renderVideoFrameJpeg(chosen);
	} finally {
		(matched as VideoFrame | null)?.close();
		(fallback as VideoFrame | null)?.close();
		if (decoder.state !== 'closed') decoder.close();
	}
}

async function renderVideoFrameJpeg(frame: VideoFrame): Promise<Blob> {
	if (typeof OffscreenCanvas !== 'undefined') {
		const canvas = new OffscreenCanvas(frame.displayWidth, frame.displayHeight);
		const ctx = canvas.getContext('2d');
		if (!ctx) throw new Error('Unable to create preview canvas.');
		ctx.drawImage(frame, 0, 0);
		return canvas.convertToBlob({ type: 'image/jpeg', quality: 0.82 });
	}
	const canvas = document.createElement('canvas');
	canvas.width = frame.displayWidth;
	canvas.height = frame.displayHeight;
	const ctx = canvas.getContext('2d');
	if (!ctx) throw new Error('Unable to create preview canvas.');
	ctx.drawImage(frame, 0, 0);
	return new Promise((resolve, reject) =>
		canvas.toBlob(
			(blob) => (blob ? resolve(blob) : reject(new Error('Unable to encode thumbnail.'))),
			'image/jpeg',
			0.82
		)
	);
}
