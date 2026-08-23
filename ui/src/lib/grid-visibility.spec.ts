import { describe, expect, it } from 'vitest';
import { measureGridVisibility } from './grid-visibility';

describe('measureGridVisibility', () => {
	it('reports fully, partially, and nearby offscreen tiles', () => {
		expect(
			measureGridVisibility(
				'full',
				{ top: 10, right: 210, bottom: 110, left: 10, width: 200, height: 100 },
				800,
				600
			)
		).toMatchObject({ visibleFraction: 1, distanceFromViewportPx: 0 });
		expect(
			measureGridVisibility(
				'partial',
				{ top: 550, right: 200, bottom: 650, left: 0, width: 200, height: 100 },
				800,
				600
			).visibleFraction
		).toBe(0.5);
		expect(
			measureGridVisibility(
				'near',
				{ top: 700, right: 200, bottom: 800, left: 0, width: 200, height: 100 },
				800,
				600
			)
		).toMatchObject({ visibleFraction: 0, distanceFromViewportPx: 100 });
	});
});
