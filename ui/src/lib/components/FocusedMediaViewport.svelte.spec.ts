import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import Fixture from './FocusedMediaViewport.fixture.svelte';

describe('FocusedMediaViewport', () => {
	it.each([
		{ width: 640, height: 360 },
		{ width: 320, height: 180 },
		{ width: 320, height: 640 }
	])(
		'keeps the zoom toolbar at the top left outside the picture at $width x $height',
		async (size) => {
			const view = await render(Fixture, { props: size });
			const toolbar = page.getByRole('group', { name: 'Digital zoom controls', exact: true });
			const viewport = page.getByRole('application', { name: 'Digital zoom viewport' });
			const outer = view.container.querySelector('[data-focused-media]')!;
			await expect
				.poll(() => {
					const toolbarBounds = toolbar.element().getBoundingClientRect();
					const mediaBounds = viewport.element().getBoundingClientRect();
					return toolbarBounds.bottom <= mediaBounds.top;
				})
				.toBe(true);
			const toolbarBounds = toolbar.element().getBoundingClientRect();
			const outerBounds = outer.getBoundingClientRect();
			expect(toolbarBounds.left - outerBounds.left).toBeLessThanOrEqual(12);
			expect(toolbarBounds.top - outerBounds.top).toBeLessThanOrEqual(12);
			await userEvent.click(page.getByRole('button', { name: 'Digital zoom in', exact: true }));
			await expect.element(page.getByLabelText('Digital zoom level')).toHaveTextContent('2.0x');
			expect(toolbar.element().getBoundingClientRect().bottom).toBeLessThanOrEqual(
				viewport.element().getBoundingClientRect().top
			);
		}
	);

	it('exposes named controls, the zoom value, disabled bounds, and 44-pixel targets', async () => {
		await render(Fixture);
		const zoomIn = page.getByRole('button', { name: 'Digital zoom in', exact: true });
		const zoomOut = page.getByRole('button', { name: 'Digital zoom out', exact: true });
		const reset = page.getByRole('button', { name: 'Reset digital zoom', exact: true });
		await expect.element(zoomOut).toBeDisabled();
		await expect.element(reset).toBeDisabled();
		expect(zoomIn.element().getBoundingClientRect().width).toBeGreaterThanOrEqual(44);
		expect(zoomIn.element().getBoundingClientRect().height).toBeGreaterThanOrEqual(44);
		for (let step = 0; step < 3; step += 1) await userEvent.click(zoomIn);
		await expect.element(page.getByLabelText('Digital zoom level')).toHaveTextContent('8.0x');
		await expect.element(zoomIn).toBeDisabled();
		await userEvent.click(reset);
		await expect.element(page.getByLabelText('Digital zoom level')).toHaveTextContent('1.0x');
	});

	it('preserves the media node and inspection state on pause, but resets for a new media identity', async () => {
		const view = await render(Fixture);
		const media = view.container.querySelector('[data-media-pixels]');
		await userEvent.click(page.getByRole('button', { name: 'Digital zoom in', exact: true }));
		await view.rerender({ playing: true });
		await expect.element(page.getByLabelText('Digital zoom level')).toHaveTextContent('2.0x');
		expect(view.container.querySelector('[data-media-pixels]')).toBe(media);
		await view.rerender({ mediaKey: 'camera-b', playing: false });
		await expect.element(page.getByLabelText('Digital zoom level')).toHaveTextContent('1.0x');
		expect(view.container.querySelector('[data-media-pixels]')).toBe(media);
	});

	it('keeps media-coordinate overlays aligned and controls unscaled after zoom and keyboard pan', async () => {
		const view = await render(Fixture, { props: { width: 800, height: 600 } });
		const zoomIn = page.getByRole('button', { name: 'Digital zoom in', exact: true });
		const originalControl = zoomIn.element().getBoundingClientRect();
		await userEvent.click(zoomIn);
		const viewport = page.getByRole('application', { name: 'Digital zoom viewport' });
		await userEvent.click(viewport);
		await userEvent.keyboard('{ArrowRight}{ArrowDown}');
		await expect
			.poll(() => {
				const layer = view.container.querySelector<HTMLElement>('[data-focused-media-layer]');
				return layer ? new DOMMatrixReadOnly(getComputedStyle(layer).transform).e : 0;
			})
			.toBe(40);
		const media = view.container.querySelector('[data-media-pixels]')!.getBoundingClientRect();
		const box = view.container.querySelector('[data-test-box]')!.getBoundingClientRect();
		expect(box.left).toBeCloseTo(media.left + media.width / 4);
		expect(box.top).toBeCloseTo(media.top + media.height / 4);
		expect(box.right).toBeCloseTo(media.left + media.width / 2);
		expect(box.bottom).toBeCloseTo(media.top + media.height / 2);
		expect(zoomIn.element().getBoundingClientRect().width).toBe(originalControl.width);
		expect(zoomIn.element().getBoundingClientRect().height).toBe(originalControl.height);
	});

	it('fits portrait media after resize and leaves physical PTZ commands independent', async () => {
		const onptz = vi.fn();
		const view = await render(Fixture, { props: { onptz } });
		await userEvent.click(page.getByRole('button', { name: 'Digital zoom in', exact: true }));
		expect(onptz).not.toHaveBeenCalled();
		await view.rerender({ width: 320, height: 640, aspectRatio: 9 / 16 });
		const viewport = page.getByRole('application', { name: 'Digital zoom viewport' });
		await expect
			.poll(() => viewport.element().getBoundingClientRect().height)
			.toBeCloseTo(568.89, 1);
		await userEvent.click(page.getByRole('button', { name: 'Physical PTZ zoom', exact: true }));
		expect(onptz).toHaveBeenCalledOnce();
		await expect.element(page.getByLabelText('Digital zoom level')).toHaveTextContent('2.0x');
		await userEvent.click(page.getByRole('button', { name: 'Reset digital zoom', exact: true }));
		await expect.element(page.getByLabelText('Digital zoom level')).toHaveTextContent('1.0x');
	});

	it('does not install inspection controls or consume gestures in compact tiles', async () => {
		const view = await render(Fixture, { props: { enabled: false } });
		expect(view.container.querySelector('[data-focused-media-layer]')).toBeNull();
		expect(view.container.querySelector('[aria-label="Digital zoom controls"]')).toBeNull();
		expect(view.container.querySelector('[data-media-pixels]')).not.toBeNull();
	});

	it('cancels an in-progress pinch when the fitted viewport resizes', async () => {
		const view = await render(Fixture);
		await userEvent.click(page.getByRole('button', { name: 'Digital zoom in', exact: true }));
		const viewport = page.getByRole('application', { name: 'Digital zoom viewport' }).element();
		const layer = view.container.querySelector<HTMLElement>('[data-focused-media-layer]')!;
		for (const pointerId of [1, 2]) {
			viewport.dispatchEvent(
				new PointerEvent('pointerdown', {
					pointerId,
					pointerType: 'touch',
					clientX: 250 + pointerId * 50,
					clientY: 180
				})
			);
		}
		await view.rerender({ width: 320, height: 180 });
		await expect.poll(() => viewport.getBoundingClientRect().height).toBe(110);
		await new Promise(requestAnimationFrame);
		const before = layer.style.transform;
		viewport.dispatchEvent(
			new PointerEvent('pointermove', {
				pointerId: 2,
				pointerType: 'touch',
				clientX: 600,
				clientY: 300
			})
		);
		await new Promise(requestAnimationFrame);
		expect(layer.style.transform).toBe(before);
		await expect.element(page.getByLabelText('Digital zoom level')).toHaveTextContent('2.0x');
	});
});
