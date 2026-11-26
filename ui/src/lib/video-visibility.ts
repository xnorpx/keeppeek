const PROBE_WIDTH = 16;
const PROBE_HEIGHT = 9;
const MIN_VISIBLE_LUMINANCE = 8;

let probeContext: CanvasRenderingContext2D | null | undefined;

function getProbeContext(): CanvasRenderingContext2D | null {
	if (probeContext !== undefined) return probeContext;
	// ponytail: reuse one offscreen 16x9 canvas to avoid allocating a DOM node per video frame.
	const canvas = document.createElement('canvas');
	canvas.width = PROBE_WIDTH;
	canvas.height = PROBE_HEIGHT;
	probeContext = canvas.getContext('2d', { willReadFrequently: true });
	return probeContext;
}

export function videoFrameIsVisible(video: HTMLVideoElement): boolean {
	if (video.videoWidth <= 0 || video.videoHeight <= 0 || video.readyState < 2) return false;
	const context = getProbeContext();
	if (!context) return false;
	try {
		context.drawImage(video, 0, 0, PROBE_WIDTH, PROBE_HEIGHT);
		const pixels = context.getImageData(0, 0, PROBE_WIDTH, PROBE_HEIGHT).data;
		let total = 0;
		for (let index = 0; index < pixels.length; index += 4) {
			total += pixels[index] + pixels[index + 1] + pixels[index + 2];
		}
		return total / (PROBE_WIDTH * PROBE_HEIGHT * 3) > MIN_VISIBLE_LUMINANCE;
	} catch {
		return false;
	}
}
