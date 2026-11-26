import type { EncodedEventKeyframe } from './control-client';

const MAX_SUPPORT_CACHE_ENTRIES = 32;
const supportCache = new Map<string, Promise<boolean>>();

let reusableDecoder: VideoDecoder | null = null;
let reusableConfigKey: string | null = null;
let pendingFrameResolve: ((frame: VideoFrame) => void) | null = null;
let pendingFrameReject: ((error: Error) => void) | null = null;
let decodeChain: Promise<unknown> = Promise.resolve();
let reusableCanvas: OffscreenCanvas | null = null;

function decoderConfigKey(media: EncodedEventKeyframe): string {
	const bytes = media.decoderConfig;
	let hash = 2_166_136_261;
	for (let index = 0; index < bytes.length; index += 1) {
		hash ^= bytes[index]!;
		hash = Math.imul(hash, 16_777_619);
	}
	return `${media.codec}:${media.width}x${media.height}:${bytes.length}:${hash >>> 0}`;
}

async function ensureConfigSupported(
	configKey: string,
	config: VideoDecoderConfig,
	codec: string
): Promise<void> {
	let cached = supportCache.get(configKey);
	if (!cached) {
		cached = VideoDecoder.isConfigSupported(config)
			.then((support) => Boolean(support.supported))
			.catch(() => false);
		supportCache.set(configKey, cached);
		while (supportCache.size > MAX_SUPPORT_CACHE_ENTRIES) {
			const oldest = supportCache.keys().next().value as string | undefined;
			if (!oldest) break;
			supportCache.delete(oldest);
		}
	}
	const supported = await cached;
	if (!supported) {
		supportCache.delete(configKey);
		throw new Error(`Browser cannot decode ${codec} event previews.`);
	}
}

function resetReusableDecoder(): void {
	const current = reusableDecoder;
	reusableDecoder = null;
	reusableConfigKey = null;
	pendingFrameResolve = null;
	pendingFrameReject = null;
	if (current && current.state !== 'closed') {
		try {
			current.close();
		} catch {
			// Ignore close errors after decoder reset.
		}
	}
}

function enqueueDecode<T>(task: () => Promise<T>): Promise<T> {
	const run = decodeChain.then(task, task);
	decodeChain = run.then(
		() => undefined,
		() => undefined
	);
	return run;
}

async function decodeKeyframeToVideoFrame(media: EncodedEventKeyframe): Promise<VideoFrame> {
	if (typeof VideoDecoder === 'undefined') {
		throw new Error('WebCodecs VideoDecoder is unavailable.');
	}
	const config: VideoDecoderConfig = {
		codec: media.codec,
		codedWidth: media.width,
		codedHeight: media.height,
		description: media.decoderConfig
	};
	const configKey = decoderConfigKey(media);
	await ensureConfigSupported(configKey, config, media.codec);

	// ponytail: Serialize keyframe decodes on one reusable VideoDecoder to avoid per-scrub codec teardown.
	return enqueueDecode(async () => {
		if (!reusableDecoder || reusableDecoder.state === 'closed') {
			reusableConfigKey = null;
			reusableDecoder = new VideoDecoder({
				output(output) {
					const resolve = pendingFrameResolve;
					if (!resolve) {
						output.close();
						return;
					}
					pendingFrameResolve = null;
					pendingFrameReject = null;
					resolve(output);
				},
				error(error) {
					const reject = pendingFrameReject;
					pendingFrameResolve = null;
					pendingFrameReject = null;
					resetReusableDecoder();
					reject?.(error);
				}
			});
		}

		const decoder = reusableDecoder;
		let frameResolved = false;
		const framePromise = new Promise<VideoFrame>((resolve, reject) => {
			pendingFrameResolve = (frame) => {
				frameResolved = true;
				resolve(frame);
			};
			pendingFrameReject = reject;
		});

		try {
			if (reusableConfigKey !== configKey || decoder.state !== 'configured') {
				decoder.configure(config);
				reusableConfigKey = configKey;
			}
			decoder.decode(
				new EncodedVideoChunk({
					type: 'key',
					timestamp: 0,
					data: media.payload
				})
			);
			return await withTimeout(
				Promise.all([decoder.flush(), framePromise]).then(([, decodedFrame]) => decodedFrame),
				2_000,
				'Event keyframe decoder produced no frame.'
			);
		} catch (cause) {
			if (!frameResolved) {
				pendingFrameResolve = null;
				pendingFrameReject = null;
			}
			resetReusableDecoder();
			throw cause;
		}
	});
}

export async function decodeEventKeyframeBitmap(media: EncodedEventKeyframe): Promise<ImageBitmap> {
	if (typeof createImageBitmap === 'undefined') {
		throw new Error('ImageBitmap decoding is unavailable.');
	}
	const output = await decodeKeyframeToVideoFrame(media);
	try {
		return await createImageBitmap(output);
	} finally {
		output.close();
	}
}

export async function decodeEventKeyframePreview(media: EncodedEventKeyframe): Promise<string> {
	const output = await decodeKeyframeToVideoFrame(media);
	try {
		const blob = await videoFrameBlob(output);
		return URL.createObjectURL(blob);
	} finally {
		output.close();
	}
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	try {
		return await Promise.race([
			promise,
			new Promise<T>((_, reject) => {
				timer = setTimeout(() => reject(new Error(message)), timeoutMs);
			})
		]);
	} finally {
		if (timer) clearTimeout(timer);
	}
}

async function videoFrameBlob(frame: VideoFrame): Promise<Blob> {
	if (typeof OffscreenCanvas !== 'undefined') {
		if (
			!reusableCanvas ||
			reusableCanvas.width !== frame.displayWidth ||
			reusableCanvas.height !== frame.displayHeight
		) {
			reusableCanvas = new OffscreenCanvas(frame.displayWidth, frame.displayHeight);
		}
		const context = reusableCanvas.getContext('2d');
		if (!context) throw new Error('Unable to create the event preview canvas.');
		context.drawImage(frame, 0, 0);
		return reusableCanvas.convertToBlob({ type: 'image/jpeg', quality: 0.84 });
	}
	const canvas = document.createElement('canvas');
	canvas.width = frame.displayWidth;
	canvas.height = frame.displayHeight;
	const context = canvas.getContext('2d');
	if (!context) throw new Error('Unable to create the event preview canvas.');
	context.drawImage(frame, 0, 0);
	return new Promise((resolve, reject) =>
		canvas.toBlob(
			(blob) => (blob ? resolve(blob) : reject(new Error('Unable to encode the event preview.'))),
			'image/jpeg',
			0.84
		)
	);
}
