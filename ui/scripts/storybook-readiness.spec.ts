import { describe, expect, test } from 'bun:test';
import {
	waitForStorybookIndexBeforeExtract,
	type StorybookPreviewRuntime
} from '../visual-harness/storybook-readiness';

describe('waitForStorybookIndexBeforeExtract', () => {
	test('holds Loki extraction until the Storybook index is ready', async () => {
		let releaseReady!: () => void;
		const ready = new Promise<void>((resolve) => {
			releaseReady = resolve;
		});
		const events: string[] = [];
		const storybookPreview: StorybookPreviewRuntime = {
			ready: async () => {
				events.push('waiting');
				await ready;
				events.push('ready');
			},
			extract: async (...args) => {
				events.push('extract');
				return args;
			}
		};

		waitForStorybookIndexBeforeExtract(storybookPreview);
		waitForStorybookIndexBeforeExtract(storybookPreview);
		const extraction = storybookPreview.extract('stories');
		await Promise.resolve();

		expect(events).toEqual(['waiting']);
		releaseReady();
		expect(await extraction).toEqual(['stories']);
		expect(events).toEqual(['waiting', 'ready', 'extract']);
	});
});
