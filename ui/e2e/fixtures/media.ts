import type { Locator } from '@playwright/test';

export async function presentMockVideoFrame(
	video: Locator,
	width = 640,
	height = 360
): Promise<void> {
	await video.evaluate(
		(element, dimensions) => {
			if (!(element instanceof HTMLVideoElement)) throw new Error('Expected a video element');
			Object.defineProperties(element, {
				videoWidth: { configurable: true, value: dimensions.width },
				videoHeight: { configurable: true, value: dimensions.height },
				readyState: { configurable: true, value: HTMLMediaElement.HAVE_CURRENT_DATA }
			});
			const originalGetContext = HTMLCanvasElement.prototype.getContext;
			Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', {
				configurable: true,
				value() {
					return {
						drawImage() {},
						getImageData() {
							return { data: new Uint8ClampedArray(16 * 9 * 4).fill(64) };
						}
					};
				}
			});
			try {
				element.dispatchEvent(new Event('playing'));
			} finally {
				Object.defineProperty(HTMLCanvasElement.prototype, 'getContext', {
					configurable: true,
					value: originalGetContext
				});
			}
		},
		{ width, height }
	);
}
