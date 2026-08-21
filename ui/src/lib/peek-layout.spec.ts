import { describe, expect, it } from 'vitest';
import {
	addPeekLayoutCamera,
	applyPeekLayoutPreset,
	createPeekLayoutDraft,
	movePeekLayoutItem,
	resizePeekLayoutItem,
	setPeekLayoutActivityFocus,
	setPeekLayoutPinned
} from './peek-layout';

const cameraIds = ['front-door', 'driveway', 'back-yard', 'porch'];

describe('Peek layout drafts', () => {
	it('creates the authored 1 + 3 layout with a middle drop target', () => {
		const draft = createPeekLayoutDraft(cameraIds.slice(0, 3));

		expect(draft.items).toEqual([
			{
				cameraId: 'front-door',
				column: 1,
				row: 1,
				columnSpan: 8,
				rowSpan: 12,
				pinned: true
			},
			{
				cameraId: 'driveway',
				column: 9,
				row: 1,
				columnSpan: 4,
				rowSpan: 4,
				pinned: false
			},
			{
				cameraId: 'back-yard',
				column: 9,
				row: 9,
				columnSpan: 4,
				rowSpan: 4,
				pinned: false
			}
		]);
	});

	it('places an unassigned camera in the first open 4 by 4 region', () => {
		const draft = createPeekLayoutDraft(cameraIds.slice(0, 3));
		const next = addPeekLayoutCamera(draft, 'porch');

		expect(next.items.at(-1)).toMatchObject({
			cameraId: 'porch',
			column: 9,
			row: 5,
			columnSpan: 4,
			rowSpan: 4
		});
		expect(draft.items).toHaveLength(3);
	});

	it('snaps movement to the grid while rejecting collisions', () => {
		const draft = createPeekLayoutDraft(cameraIds.slice(0, 3));
		const moved = movePeekLayoutItem(draft, 'driveway', 9.2, 2.4);
		const rejected = movePeekLayoutItem(moved, 'driveway', 8, 5);

		expect(moved.items.find((item) => item.cameraId === 'driveway')).toMatchObject({
			column: 9,
			row: 2
		});
		expect(moved.preset).toBe('custom');
		expect(rejected).toBe(moved);
	});

	it('resizes on the grid while rejecting overlap with another tile', () => {
		const draft = createPeekLayoutDraft(cameraIds.slice(0, 3));
		const resized = resizePeekLayoutItem(draft, 'front-door', 7, 11);
		const rejected = resizePeekLayoutItem(resized, 'front-door', 9, 12);

		expect(resized.items.find((item) => item.cameraId === 'front-door')).toMatchObject({
			columnSpan: 7,
			rowSpan: 11
		});
		expect(resized.preset).toBe('custom');
		expect(rejected).toBe(resized);
	});

	it('applies presets without losing activity focus or explicit pins', () => {
		let draft = createPeekLayoutDraft(cameraIds.slice(0, 3));
		draft = setPeekLayoutActivityFocus(draft, false);
		draft = setPeekLayoutPinned(draft, 'driveway', true);

		const next = applyPeekLayoutPreset(draft, cameraIds, '2x2');

		expect(next.preset).toBe('2x2');
		expect(next.activityFocus).toBe(false);
		expect(next.items).toHaveLength(4);
		expect(next.items.filter((item) => item.pinned).map((item) => item.cameraId)).toEqual([
			'front-door',
			'driveway'
		]);
	});
});
