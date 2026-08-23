import type { EncodedEventKeyframe } from './control-client';

export async function decodeEventKeyframePreview(media: EncodedEventKeyframe): Promise<string> {
	if (typeof VideoDecoder === 'undefined') {
		throw new Error('WebCodecs VideoDecoder is unavailable.');
	}
	const config: VideoDecoderConfig = {
		codec: media.codec,
		codedWidth: media.width,
		codedHeight: media.height,
		description: media.decoderConfig
	};
	const support = await VideoDecoder.isConfigSupported(config);
	if (!support.supported) throw new Error(`Browser cannot decode ${media.codec} event previews.`);

	let resolveFrame!: (frame: VideoFrame) => void;
	let rejectFrame!: (error: Error) => void;
	const frame = new Promise<VideoFrame>((resolve, reject) => {
		resolveFrame = resolve;
		rejectFrame = reject;
	});
	let receivedFrame = false;
	const decoder = new VideoDecoder({
		output(output) {
			if (receivedFrame) {
				output.close();
				return;
			}
			receivedFrame = true;
			resolveFrame(output);
		},
		error(error) {
			rejectFrame(error);
		}
	});

	try {
		decoder.configure(config);
		decoder.decode(
			new EncodedVideoChunk({
				type: 'key',
				timestamp: 0,
				data: media.payload
			})
		);
		const output = await withTimeout(
			Promise.all([decoder.flush(), frame]).then(([, decodedFrame]) => decodedFrame),
			2_000,
			'Event keyframe decoder produced no frame.'
		);
		try {
			const blob = await videoFrameBlob(output);
			return URL.createObjectURL(blob);
		} finally {
			output.close();
		}
	} finally {
		if (decoder.state !== 'closed') decoder.close();
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
		const canvas = new OffscreenCanvas(frame.displayWidth, frame.displayHeight);
		const context = canvas.getContext('2d');
		if (!context) throw new Error('Unable to create the event preview canvas.');
		context.drawImage(frame, 0, 0);
		return canvas.convertToBlob({ type: 'image/jpeg', quality: 0.84 });
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
