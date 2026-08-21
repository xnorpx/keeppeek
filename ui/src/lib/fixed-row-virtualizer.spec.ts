import { describe, expect, it } from 'vitest';
import { fixedRowWindow } from './fixed-row-virtualizer';

const options = { rowHeight: 56, overscan: 4, maxItems: 24 };

describe('fixedRowWindow', () => {
	it('renders a bounded first window for 127 sources', () => {
		expect(fixedRowWindow(127, 0, 560, options)).toEqual({
			startIndex: 0,
			endIndex: 18,
			offsetTop: 0,
			totalHeight: 7_112
		});
	});

	it('tracks a middle scroll position with overscan', () => {
		const window = fixedRowWindow(127, 2_800, 560, options);

		expect(window).toMatchObject({ startIndex: 46, endIndex: 64, offsetTop: 2_576 });
		expect(window.endIndex - window.startIndex).toBeLessThanOrEqual(24);
	});

	it('keeps the final row mounted at the bottom', () => {
		const window = fixedRowWindow(127, 20_000, 560, options);

		expect(window.endIndex).toBe(127);
		expect(window.endIndex - window.startIndex).toBe(18);
	});

	it('returns an empty zero-height window', () => {
		expect(fixedRowWindow(0, 500, 500, options)).toEqual({
			startIndex: 0,
			endIndex: 0,
			offsetTop: 0,
			totalHeight: 0
		});
	});
});
