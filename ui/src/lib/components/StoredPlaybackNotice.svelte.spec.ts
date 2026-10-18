import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import StoredPlaybackNotice from './StoredPlaybackNotice.svelte';

describe('stored playback notice', () => {
	it('keeps unclassified preparation generic and offers cancellation on a narrow screen', async () => {
		await page.viewport(320, 844);
		const close = vi.fn();
		const { container } = await render(StoredPlaybackNotice, {
			archive: false,
			pending: true,
			error: null,
			onretry: vi.fn(),
			onclose: close
		});
		await expect.element(page.getByText('Preparing playback…', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Preparing archive playback…', { exact: true }))
			.not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Close playback', exact: true }).click();
		expect(close).toHaveBeenCalledOnce();
		expect(container.scrollWidth).toBeLessThanOrEqual(320);
	});
	it('shows the known archive failure with retry and close actions', async () => {
		const retry = vi.fn();
		const close = vi.fn();
		await render(StoredPlaybackNotice, {
			archive: true,
			pending: false,
			error: 'The requested media is unavailable.',
			onretry: retry,
			onclose: close
		});
		await expect.element(page.getByRole('alert')).toBeVisible();
		await expect
			.element(page.getByText('Archive playback unavailable', { exact: true }))
			.toBeVisible();
		await page.getByRole('button', { name: 'Retry playback', exact: true }).click();
		await page.getByRole('button', { name: 'Close playback', exact: true }).click();
		expect(retry).toHaveBeenCalledOnce();
		expect(close).toHaveBeenCalledOnce();
	});
});
