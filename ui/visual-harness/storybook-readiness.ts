export type StorybookPreviewRuntime = {
	ready: () => Promise<void>;
	extract: (...args: unknown[]) => Promise<unknown>;
};

const guardedPreviews = new WeakSet<StorybookPreviewRuntime>();

export function waitForStorybookIndexBeforeExtract(
	storybookPreview: StorybookPreviewRuntime | undefined
): void {
	if (!storybookPreview || guardedPreviews.has(storybookPreview)) return;

	const extract = storybookPreview.extract.bind(storybookPreview);
	storybookPreview.extract = async (...args) => {
		await storybookPreview.ready();
		return extract(...args);
	};
	guardedPreviews.add(storybookPreview);
}
