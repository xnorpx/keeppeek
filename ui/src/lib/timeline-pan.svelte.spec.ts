import { afterEach, describe, expect, it, vi } from 'vitest';
import { TimelinePan } from './timeline-pan.svelte';

function pointer(type: string, pointerId: number, clientX: number, clientY: number): PointerEvent {
	return new PointerEvent(type, {
		bubbles: true,
		button: 0,
		clientX,
		clientY,
		pointerId
	});
}

function scrollTarget() {
	return {
		scrollLeft: 120,
		scrollTop: 240,
		setPointerCapture: vi.fn(),
		hasPointerCapture: vi.fn(() => true),
		releasePointerCapture: vi.fn()
	} as unknown as HTMLElement;
}

describe('TimelinePan', () => {
	afterEach(() => {
		vi.useRealTimers();
	});

	it('pans both axes after a hold and suppresses the release click', () => {
		vi.useFakeTimers();
		const target = scrollTarget();
		const pan = new TimelinePan();

		pan.begin(pointer('pointerdown', 1, 100, 100), target);
		vi.advanceTimersByTime(350);
		pan.move(pointer('pointermove', 1, 40, 25));

		expect(pan.active).toBe(true);
		expect(target.setPointerCapture).toHaveBeenCalledWith(1);
		expect(pan.end(pointer('pointerup', 1, 40, 25))).toBe(true);
		expect(target.scrollLeft).toBe(180);
		expect(target.scrollTop).toBe(315);

		vi.advanceTimersByTime(200);
		const click = new MouseEvent('click', { cancelable: true });
		pan.consumeClick(click);
		expect(click.defaultPrevented).toBe(true);

		vi.advanceTimersByTime(50);
		const laterClick = new MouseEvent('click', { cancelable: true });
		pan.consumeClick(laterClick);
		expect(laterClick.defaultPrevented).toBe(false);
	});

	it('leaves a short press available for normal seek clicks', () => {
		vi.useFakeTimers();
		const target = scrollTarget();
		const pan = new TimelinePan();

		pan.begin(pointer('pointerdown', 1, 100, 100), target);
		vi.advanceTimersByTime(200);

		expect(pan.end(pointer('pointerup', 1, 100, 100))).toBe(false);
		const click = new MouseEvent('click', { cancelable: true });
		pan.consumeClick(click);
		expect(click.defaultPrevented).toBe(false);
	});
});
