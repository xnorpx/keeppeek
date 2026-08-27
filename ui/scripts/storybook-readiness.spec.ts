import { describe, expect, test } from 'bun:test';
import {
	installStorybookReadinessGuard,
	waitForStorybookIndexBeforeExtract,
	type LokiReadyStateHost,
	type StorybookPreviewRuntime
} from '../visual-harness/storybook-readiness';

type DeferredIndex = {
	preview: StorybookPreviewRuntime;
	events: string[];
	releaseIndex: () => void;
};

function createStorybookPreview(): DeferredIndex {
	const events: string[] = [];
	let releaseIndex!: () => void;
	const indexed = new Promise<void>((resolve) => {
		releaseIndex = resolve;
	});
	let storyStoreValue: { raw: () => string[] } | undefined;

	class PreviewWeb {
		get storyStore() {
			return new Proxy(
				{},
				{
					get: (_target, property) => {
						if (!storyStoreValue) throw new Error('SB_PREVIEW_API_0011');
						return Reflect.get(storyStoreValue, property);
					}
				}
			);
		}

		async ready() {
			events.push('waiting');
			await indexed;
			storyStoreValue = { raw: () => ['story'] };
			events.push('ready');
		}

		async extract(...args: unknown[]) {
			if (!storyStoreValue) throw new Error('SB_PREVIEW_API_0011');
			events.push('extract');
			return args;
		}
	}

	return { preview: new PreviewWeb() as StorybookPreviewRuntime, events, releaseIndex };
}

describe('waitForStorybookIndexBeforeExtract', () => {
	test('holds Loki extraction until the Storybook index is ready', async () => {
		const { preview, events, releaseIndex } = createStorybookPreview();

		waitForStorybookIndexBeforeExtract(preview);
		waitForStorybookIndexBeforeExtract(preview);
		const extraction = preview.extract('stories');
		await Promise.resolve();

		expect(events).toEqual(['waiting']);
		releaseIndex();
		expect(await extraction).toEqual(['stories']);
		expect(events).toEqual(['waiting', 'ready', 'extract']);
	});

	test("answers Loki's synchronous story store probe before the index is ready", async () => {
		const { preview, releaseIndex } = createStorybookPreview();
		const storyStoreOf = (guarded: StorybookPreviewRuntime) =>
			(guarded as unknown as { storyStore: { raw: () => string[] } }).storyStore;

		expect(() => storyStoreOf(preview).raw).toThrow('SB_PREVIEW_API_0011');
		waitForStorybookIndexBeforeExtract(preview);
		expect(storyStoreOf(preview).raw).toBeInstanceOf(Function);
		expect(() => storyStoreOf(preview).raw()).toThrow(
			'The Storybook story store was used before its index finished initializing.'
		);

		releaseIndex();
		await preview.extract();
		expect(storyStoreOf(preview).raw()).toEqual(['story']);
	});
});

describe('installStorybookReadinessGuard', () => {
	test('guards the preview whether it is assigned before or after installation', async () => {
		for (const assignBeforeInstall of [true, false]) {
			const { preview, events, releaseIndex } = createStorybookPreview();
			const host: LokiReadyStateHost = {};

			if (assignBeforeInstall) host.__STORYBOOK_PREVIEW__ = preview;
			installStorybookReadinessGuard(host);
			if (!assignBeforeInstall) host.__STORYBOOK_PREVIEW__ = preview;

			expect(host.__STORYBOOK_PREVIEW__).toBe(preview);
			const extraction = host.__STORYBOOK_PREVIEW__!.extract('stories');
			await Promise.resolve();
			expect(events).toEqual(['waiting']);

			releaseIndex();
			expect(await extraction).toEqual(['stories']);
		}
	});

	test('holds Loki page load until the Storybook index is ready', async () => {
		const { preview, releaseIndex } = createStorybookPreview();
		const pendingPromises: Promise<unknown>[] = [];
		const host: LokiReadyStateHost = {
			loki: { registerPendingPromise: (pending) => pendingPromises.push(pending) }
		};

		installStorybookReadinessGuard(host);
		expect(pendingPromises).toHaveLength(1);

		let lokiReady = false;
		const awaitReady = Promise.all(pendingPromises).then(() => {
			lokiReady = true;
		});

		host.__STORYBOOK_PREVIEW__ = preview;
		await Promise.resolve();
		expect(lokiReady).toBe(false);

		releaseIndex();
		await awaitReady;
		expect(lokiReady).toBe(true);
	});

	test('releases Loki page load when the preview never appears', async () => {
		const pendingPromises: Promise<unknown>[] = [];
		const host: LokiReadyStateHost = {
			loki: { registerPendingPromise: (pending) => pendingPromises.push(pending) }
		};

		installStorybookReadinessGuard(host, 1);
		await Promise.all(pendingPromises);
	});
});
