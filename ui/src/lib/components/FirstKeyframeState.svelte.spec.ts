import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import FirstKeyframeState from './FirstKeyframeState.svelte';

const cachedFrame =
	'data:image/gif;base64,R0lGODlhAQABAPAAAP///wAAACH5BAAAAAAALAAAAAABAAEAAAICRAEAOw==';

describe('FirstKeyframeState', () => {
	it('shows a cached camera frame while live video waits for a keyframe', async () => {
		const { container } = await render(FirstKeyframeState, {
			props: { label: 'North Frontyard', elapsedMs: 7_800, frameUrl: cachedFrame }
		});

		const image = container.querySelector<HTMLImageElement>('[data-peek-cached-frame]');
		expect(image?.src).toBe(cachedFrame);
		expect(getComputedStyle(image!).objectFit).toBe('contain');
		await expect.element(page.getByText('RESTORING', { exact: true })).toBeVisible();
		await expect
			.element(
				page.getByText('Showing last frame · waiting for live video · 7.8s', { exact: true })
			)
			.toBeVisible();
	});

	it('uses an explicitly selected crop policy for cached frames', async () => {
		const { container } = await render(FirstKeyframeState, {
			props: { label: 'Front', elapsedMs: 100, frameUrl: cachedFrame, mediaFit: 'cover' }
		});
		const image = container.querySelector<HTMLImageElement>('[data-peek-cached-frame]');
		expect(getComputedStyle(image!).objectFit).toBe('cover');
	});

	it('keeps the initial connection state when no cached frame exists', async () => {
		await render(FirstKeyframeState, {
			props: { label: 'North Frontyard', elapsedMs: 400 }
		});

		await expect.element(page.getByText('CONNECTING', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Negotiated · waiting for a keyframe · 0.4s', { exact: true }))
			.toBeVisible();
	});
});
