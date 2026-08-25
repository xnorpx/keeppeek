import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import SharedAccessKeyControl from './SharedAccessKeyControl.svelte';

describe('SharedAccessKeyControl', () => {
	afterEach(() => vi.restoreAllMocks());

	it('reveals explicitly, copies the visible key, and confirms before rotation', async () => {
		const reveal = vi.fn().mockResolvedValue('550e8400-e29b-41d4-a716-446655440000');
		const rotate = vi.fn().mockResolvedValue('3d813cbb-47fb-4a95-953d-1339b8ff7f54');
		const writeText = vi.fn().mockResolvedValue(undefined);
		Object.defineProperty(navigator, 'clipboard', {
			configurable: true,
			value: { writeText }
		});
		await render(SharedAccessKeyControl, { props: { onreveal: reveal, onrotate: rotate } });

		await expect
			.element(page.getByText('550e8400-e29b-41d4-a716-446655440000', { exact: true }))
			.not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Reveal key' }).click();
		await expect
			.element(page.getByText('550e8400-e29b-41d4-a716-446655440000', { exact: true }))
			.toBeVisible();
		expect(reveal).toHaveBeenCalledOnce();

		await page.getByRole('button', { name: 'Copy key' }).click();
		expect(writeText).toHaveBeenCalledWith('550e8400-e29b-41d4-a716-446655440000');

		await page.getByRole('button', { name: 'Rotate key' }).click();
		expect(rotate).not.toHaveBeenCalled();
		await page.getByRole('button', { name: 'Confirm rotation' }).click();
		await expect
			.element(page.getByText('3d813cbb-47fb-4a95-953d-1339b8ff7f54', { exact: true }))
			.toBeVisible();
		expect(rotate).toHaveBeenCalledOnce();
	});
});
