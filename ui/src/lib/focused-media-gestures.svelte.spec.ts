import { afterEach, describe, expect, it, vi } from 'vitest';
import { createFocusedMediaControls } from './focused-media-gestures';

const cleanup: (() => void)[] = [];

afterEach(() => {
	for (const dispose of cleanup.splice(0)) dispose();
});

function fixture() {
	const viewport = document.createElement('div');
	const layer = document.createElement('div');
	viewport.style.cssText = 'position:relative;width:640px;height:360px;';
	layer.style.cssText = 'width:100%;height:100%;';
	viewport.tabIndex = 0;
	viewport.append(layer);
	document.body.append(viewport);
	const onscalechange = vi.fn();
	const controls = createFocusedMediaControls(viewport, layer, onscalechange);
	cleanup.push(() => {
		controls.destroy();
		viewport.remove();
	});
	return { viewport, layer, controls, onscalechange };
}

function transform(layer: HTMLElement): DOMMatrixReadOnly {
	return new DOMMatrixReadOnly(getComputedStyle(layer).transform);
}

function pointer(
	viewport: HTMLElement,
	type: string,
	pointerId: number,
	horizontalPx: number,
	verticalPx: number
) {
	const bounds = viewport.getBoundingClientRect();
	viewport.dispatchEvent(
		new PointerEvent(type, {
			pointerId,
			pointerType: 'touch',
			clientX: bounds.left + horizontalPx,
			clientY: bounds.top + verticalPx,
			bubbles: true,
			cancelable: true
		})
	);
}

describe('focused media gestures', () => {
	it('exposes keyboard pan ownership before the next frame for capture-phase playback shortcuts', () => {
		const { viewport, controls } = fixture();
		expect(viewport.hasAttribute('data-digital-pan-active')).toBe(false);
		controls.zoomIn();
		expect(viewport.hasAttribute('data-digital-pan-active')).toBe(true);
		controls.reset();
		expect(viewport.hasAttribute('data-digital-pan-active')).toBe(false);
	});

	it('bounds zoom from one to eight and resets translation with the scale', async () => {
		const { viewport, layer, controls } = fixture();
		for (let step = 0; step < 12; step += 1) controls.zoomIn();
		await expect.poll(() => transform(layer).a).toBe(8);
		viewport.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
		await expect.poll(() => transform(layer).e).toBeGreaterThan(0);
		controls.reset();
		await expect.poll(() => transform(layer).isIdentity).toBe(true);
		controls.zoomOut();
		await expect.poll(() => transform(layer).isIdentity).toBe(true);
	});

	it('zooms around the double-click location without moving that image coordinate', async () => {
		const { viewport, layer } = fixture();
		const bounds = viewport.getBoundingClientRect();
		viewport.dispatchEvent(
			new MouseEvent('dblclick', {
				clientX: bounds.left + 480,
				clientY: bounds.top + 270,
				bubbles: true,
				cancelable: true
			})
		);
		await expect.poll(() => transform(layer).a).toBe(2);
		expect(transform(layer).e).toBeCloseTo(-160);
		expect(transform(layer).f).toBeCloseTo(-90);
	});

	it('requires Alt for wheel zoom and preserves browser zoom shortcuts', async () => {
		const { viewport, layer } = fixture();
		const plainWheel = new WheelEvent('wheel', { deltaY: -100, cancelable: true });
		const browserWheel = new WheelEvent('wheel', {
			deltaY: -100,
			ctrlKey: true,
			altKey: true,
			cancelable: true
		});
		const browserKey = new KeyboardEvent('keydown', {
			key: '+',
			metaKey: true,
			cancelable: true
		});
		viewport.dispatchEvent(plainWheel);
		viewport.dispatchEvent(browserWheel);
		viewport.dispatchEvent(browserKey);
		expect(plainWheel.defaultPrevented).toBe(false);
		expect(browserWheel.defaultPrevented).toBe(false);
		expect(browserKey.defaultPrevented).toBe(false);
		const zoomWheel = new WheelEvent('wheel', {
			deltaY: -100,
			altKey: true,
			cancelable: true
		});
		viewport.dispatchEvent(zoomWheel);
		expect(zoomWheel.defaultPrevented).toBe(true);
		await expect.poll(() => transform(layer).a).toBeGreaterThan(1);
	});

	it('pans only when zoomed and cannot drag the image outside its viewport', async () => {
		const { viewport, layer, controls } = fixture();
		pointer(viewport, 'pointerdown', 1, 100, 100);
		pointer(viewport, 'pointermove', 1, 200, 200);
		pointer(viewport, 'pointerup', 1, 200, 200);
		expect(transform(layer).isIdentity).toBe(true);
		controls.zoomIn();
		pointer(viewport, 'pointerdown', 1, 100, 100);
		pointer(viewport, 'pointermove', 1, 2000, 2000);
		pointer(viewport, 'pointerup', 1, 2000, 2000);
		await expect.poll(() => transform(layer).e).toBe(320);
		expect(transform(layer).f).toBe(180);
	});

	it('pinches around two pointers and ignores a third pointer', async () => {
		const { viewport, layer } = fixture();
		pointer(viewport, 'pointerdown', 1, 270, 180);
		pointer(viewport, 'pointerdown', 2, 370, 180);
		pointer(viewport, 'pointerdown', 3, 600, 300);
		pointer(viewport, 'pointermove', 3, 630, 340);
		expect(transform(layer).isIdentity).toBe(true);
		pointer(viewport, 'pointermove', 2, 470, 180);
		await expect.poll(() => transform(layer).a).toBe(2);
		expect(transform(layer).e).toBe(50);
		pointer(viewport, 'pointerup', 2, 470, 180);
		pointer(viewport, 'pointermove', 1, 290, 190);
		await expect.poll(() => transform(layer).e).toBe(70);
		expect(transform(layer).f).toBe(10);
	});

	it('cancels captured gestures on reset and pointer cancellation', async () => {
		const { viewport, layer, controls } = fixture();
		controls.zoomIn();
		pointer(viewport, 'pointerdown', 1, 100, 100);
		pointer(viewport, 'pointercancel', 1, 100, 100);
		pointer(viewport, 'pointermove', 1, 200, 200);
		await expect.poll(() => transform(layer).a).toBe(2);
		expect(transform(layer).e).toBe(0);
		pointer(viewport, 'pointerdown', 1, 100, 100);
		controls.reset();
		pointer(viewport, 'pointermove', 1, 200, 200);
		await expect.poll(() => transform(layer).isIdentity).toBe(true);
	});

	it('double-taps to inspection zoom once even when the browser also emits double-click', async () => {
		const { viewport, layer } = fixture();
		for (let tap = 0; tap < 2; tap += 1) {
			pointer(viewport, 'pointerdown', 1, 480, 270);
			pointer(viewport, 'pointerup', 1, 480, 270);
		}
		await expect.poll(() => transform(layer).a).toBe(2);
		viewport.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
		await new Promise(requestAnimationFrame);
		expect(transform(layer).a).toBe(2);
		expect(transform(layer).e).toBe(-160);
	});

	it('coalesces a burst into one rendered update and restores browser behavior on teardown', async () => {
		const { viewport, layer, controls, onscalechange } = fixture();
		for (let step = 0; step < 100; step += 1) controls.zoomIn();
		await expect.poll(() => transform(layer).a).toBe(8);
		expect(onscalechange).toHaveBeenCalledExactlyOnceWith(8);
		controls.destroy();
		expect(viewport.style.touchAction).toBe('');
		expect(layer.style.transform).toBe('');
		const wheel = new WheelEvent('wheel', { altKey: true, deltaY: -100, cancelable: true });
		viewport.dispatchEvent(wheel);
		expect(wheel.defaultPrevented).toBe(false);
	});
});
