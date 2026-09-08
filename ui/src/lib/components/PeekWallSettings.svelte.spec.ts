import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import { defaultPeekWallPreferences } from '$lib/peek-wall-preferences';
import PeekWallSettings from './PeekWallSettings.svelte';

describe('Peek wall display settings', () => {
	it('keeps save and discard visible while mobile display options scroll', async () => {
		await page.viewport(390, 700);
		await render(PeekWallSettings, {
			preferences: defaultPeekWallPreferences(),
			deviceCapacity: 4,
			visibleStreams: 3,
			activeStreams: 3,
			editable: true,
			dirty: true,
			onchange: vi.fn(),
			onsave: vi.fn()
		});
		await page.getByRole('button', { name: 'Wall display settings' }).click();
		const save = document.querySelector<HTMLButtonElement>(
			'button[aria-label="Save display settings"]'
		)!;
		const bounds = save.getBoundingClientRect();
		expect(bounds.height).toBeGreaterThanOrEqual(44);
		expect(bounds.bottom).toBeLessThanOrEqual(window.innerHeight);
		await page.getByRole('button', { name: 'Wall display settings' }).click();
	});

	it('previews appearance presets and custom pixels without saving automatically', async () => {
		const onchange = vi.fn();
		const onsave = vi.fn();
		const { rerender } = await render(PeekWallSettings, {
			preferences: defaultPeekWallPreferences(),
			deviceCapacity: 4,
			visibleStreams: 3,
			activeStreams: 3,
			editable: true,
			dirty: true,
			onchange,
			onsave
		});
		await page.getByRole('button', { name: 'Wall display settings' }).click();
		await page.getByRole('radio', { name: 'Hairline', exact: true }).click();
		expect(onchange).toHaveBeenCalledWith({
			...defaultPeekWallPreferences(),
			gapPx: 2,
			cornerRadiusPx: 0
		});
		expect(onsave).not.toHaveBeenCalled();
		await page.getByRole('button', { name: 'Wall display settings' }).click();
		await rerender({
			preferences: { ...defaultPeekWallPreferences(), gapPx: 7, cornerRadiusPx: 18 }
		});
		await page.getByRole('button', { name: 'Wall display settings' }).click();
		await expect.element(page.getByText('Custom', { exact: true })).toBeVisible();
		await expect.element(page.getByRole('spinbutton', { name: 'Gap (px)' })).toHaveValue(7);
		await expect
			.element(page.getByRole('spinbutton', { name: 'Corner radius (px)' }))
			.toHaveValue(18);
		await page.getByRole('button', { name: 'Save display settings' }).click();
		expect(onsave).toHaveBeenCalledTimes(1);
		await page.getByRole('button', { name: 'Wall display settings' }).click();
	});

	it('keeps shared dashboard settings read-only without edit permission', async () => {
		await render(PeekWallSettings, {
			preferences: defaultPeekWallPreferences(),
			deviceCapacity: 4,
			visibleStreams: 2,
			activeStreams: 2,
			editable: false,
			onchange: vi.fn()
		});
		await page.getByRole('button', { name: 'Wall display settings' }).click();
		await expect.element(page.getByRole('radio', { name: '4:3', exact: true })).toBeDisabled();
		await expect.element(page.getByRole('slider', { name: 'Gap', exact: true })).toBeDisabled();
		await expect.element(page.getByText('Read-only dashboard')).toBeVisible();
		await page.getByRole('button', { name: 'Wall display settings' }).click();
	});

	it('shows save errors without discarding the preview and supports explicit discard', async () => {
		const ondiscard = vi.fn();
		await render(PeekWallSettings, {
			preferences: { ...defaultPeekWallPreferences(), gapPx: 7 },
			deviceCapacity: 4,
			visibleStreams: 2,
			activeStreams: 2,
			editable: true,
			dirty: true,
			saveError: 'Dashboard changed on the server. Reload before saving.',
			onchange: vi.fn(),
			ondiscard
		});
		await page.getByRole('button', { name: 'Wall display settings' }).click();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('Dashboard changed on the server. Reload before saving.');
		await expect.element(page.getByRole('spinbutton', { name: 'Gap (px)' })).toHaveValue(7);
		await page.getByRole('button', { name: 'Discard changes' }).click();
		expect(ondiscard).toHaveBeenCalledTimes(1);
		await page.getByRole('button', { name: 'Wall display settings' }).click();
	});

	it('shows demand before Continuous activation and emits preview changes', async () => {
		const onchange = vi.fn();
		await render(PeekWallSettings, {
			preferences: defaultPeekWallPreferences(),
			deviceCapacity: 4,
			visibleStreams: 9,
			activeStreams: 4,
			editable: true,
			onchange
		});
		await page.getByRole('button', { name: 'Wall display settings' }).click();
		await expect.element(page.getByText('9 streams / 9 decoders requested')).toBeVisible();
		await expect.element(page.getByText('5 over budget')).toBeVisible();
		await expect.element(page.getByText('Saved', { exact: true })).toBeVisible();
		await page.getByRole('radio', { name: 'Continuous', exact: true }).click();
		expect(onchange).toHaveBeenCalledWith({
			...defaultPeekWallPreferences(),
			streamingMode: 'continuous'
		});
		await page.getByRole('radio', { name: '4:3', exact: true }).click();
		expect(onchange).toHaveBeenCalledWith({ ...defaultPeekWallPreferences(), tileShape: '4:3' });
		await page.getByRole('button', { name: 'Wall display settings' }).click();
	});

	it('has mobile-sized controls, keyboard access, and a complete reset', async () => {
		await page.viewport(390, 844);
		const onchange = vi.fn();
		const { container } = await render(PeekWallSettings, {
			preferences: { ...defaultPeekWallPreferences(), tileShape: 'native', mediaFit: 'cover' },
			deviceCapacity: 4,
			visibleStreams: 2,
			activeStreams: 2,
			editable: true,
			onchange
		});
		const trigger = container.querySelector<HTMLButtonElement>('[data-peek-wall-settings] button');
		expect(trigger!.getBoundingClientRect().height).toBeGreaterThanOrEqual(44);
		expect(trigger!.getBoundingClientRect().width).toBeGreaterThanOrEqual(44);
		const frame = container.querySelector<HTMLElement>('[data-wall-settings-frame]')!;
		expect(frame.getBoundingClientRect().height).toBe(32);
		expect(frame.getBoundingClientRect().width).toBe(32);
		await page.getByRole('button', { name: 'Wall display settings' }).click();
		await expect.element(page.getByRole('radio', { name: 'Native', exact: true })).toBeChecked();
		await expect.element(page.getByRole('radio', { name: 'Cover (cropped)' })).toBeChecked();
		await page.getByRole('button', { name: 'Reset wall settings' }).click();
		expect(onchange).toHaveBeenCalledWith(defaultPeekWallPreferences());
		await page.getByRole('button', { name: 'Wall display settings' }).click();
	});
});
