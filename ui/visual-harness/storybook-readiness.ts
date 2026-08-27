export type StorybookPreviewRuntime = {
	ready: () => Promise<void>;
	extract: (...args: unknown[]) => Promise<unknown>;
};

export type LokiReadyStateHost = {
	__STORYBOOK_PREVIEW__?: StorybookPreviewRuntime;
	loki?: { registerPendingPromise?: (pending: Promise<unknown>) => void };
};

const guardedPreviews = new WeakSet<StorybookPreviewRuntime>();
const guardedHosts = new WeakSet<LokiReadyStateHost>();

export const STORYBOOK_INDEX_TIMEOUT_MS = 60_000;

function findStoryStoreGetter(preview: StorybookPreviewRuntime): (() => unknown) | undefined {
	for (
		let holder: object | null = preview;
		holder;
		holder = Object.getPrototypeOf(holder) as object | null
	) {
		const descriptor = Object.getOwnPropertyDescriptor(holder, 'storyStore');
		if (descriptor?.get) return descriptor.get;
	}
	return undefined;
}

function reportStoryStoreUsedBeforeIndex(): never {
	throw new Error('The Storybook story store was used before its index finished initializing.');
}

/**
 * Loki probes `__STORYBOOK_PREVIEW__.storyStore.raw` synchronously before awaiting `extract()`, and
 * Storybook throws `StoryStoreAccessedBeforeInitializationError` from that getter until the index is
 * ready. Answer the probe with a placeholder so extraction reaches the awaited path.
 */
function answerStoryStoreProbeBeforeIndex(preview: StorybookPreviewRuntime): void {
	const readStoryStore = findStoryStoreGetter(preview);
	if (!readStoryStore) return;

	Object.defineProperty(preview, 'storyStore', {
		configurable: true,
		get: () => {
			const storyStore = readStoryStore.call(preview) as object;
			return new Proxy(
				{},
				{
					get: (_target, property) => {
						try {
							return Reflect.get(storyStore, property);
						} catch {
							return reportStoryStoreUsedBeforeIndex;
						}
					}
				}
			);
		}
	});
}

export function waitForStorybookIndexBeforeExtract(
	storybookPreview: StorybookPreviewRuntime | undefined
): void {
	if (!storybookPreview || guardedPreviews.has(storybookPreview)) return;

	const extract = storybookPreview.extract.bind(storybookPreview);
	storybookPreview.extract = async (...args) => {
		await storybookPreview.ready();
		return extract(...args);
	};
	answerStoryStoreProbeBeforeIndex(storybookPreview);
	guardedPreviews.add(storybookPreview);
}

function settleWithin(pending: Promise<unknown>, timeoutMs: number): Promise<void> {
	return new Promise<void>((resolve) => {
		const deadline = setTimeout(resolve, timeoutMs);
		const stopWaiting = () => {
			clearTimeout(deadline);
			resolve();
		};
		void pending.then(stopWaiting, stopWaiting);
	});
}

/**
 * Storybook assigns `__STORYBOOK_PREVIEW__` in the preview constructor and initializes its index
 * afterwards, so the guard has to survive both assignment orders. Registering the index promise with
 * Loki's ready-state manager also holds page load until stories can be enumerated.
 */
export function installStorybookReadinessGuard(
	host: LokiReadyStateHost,
	timeoutMs: number = STORYBOOK_INDEX_TIMEOUT_MS
): void {
	if (guardedHosts.has(host)) return;
	guardedHosts.add(host);

	let assignedPreview = host.__STORYBOOK_PREVIEW__;
	let announcePreview!: (preview: StorybookPreviewRuntime) => void;
	const previewAvailable = new Promise<StorybookPreviewRuntime>((resolve) => {
		announcePreview = resolve;
	});

	const guard = (preview: StorybookPreviewRuntime | undefined) => {
		if (!preview) return;
		waitForStorybookIndexBeforeExtract(preview);
		announcePreview(preview);
	};

	Object.defineProperty(host, '__STORYBOOK_PREVIEW__', {
		configurable: true,
		get: () => assignedPreview,
		set: (preview: StorybookPreviewRuntime | undefined) => {
			assignedPreview = preview;
			guard(preview);
		}
	});
	guard(assignedPreview);

	host.loki?.registerPendingPromise?.(
		settleWithin(
			previewAvailable.then((preview) => preview.ready()),
			timeoutMs
		)
	);
}
