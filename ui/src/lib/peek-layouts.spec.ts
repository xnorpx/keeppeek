import { describe, expect, it } from 'vitest';
import {
	PEEK_LAYOUT_PICKER_PRESETS,
	PEEK_LAYOUT_PRESETS,
	PEEK_LAYOUT_STORAGE_KEY,
	createDefaultPeekLayoutState,
	layoutSlotPlacement,
	loadPeekLayoutState,
	normalizePeekLayoutState,
	savePeekLayoutState,
	slotCountForLayout,
	slotsForLayout
} from './peek-layouts';

describe('Peek layouts', () => {
	it('falls back to the dynamic view for malformed stored state', () => {
		expect(normalizePeekLayoutState({ version: 2, layouts: [] })).toEqual(
			createDefaultPeekLayoutState()
		);
	});

	it('keeps only unique, valid custom camera selections in grid bounds', () => {
		const state = normalizePeekLayoutState({
			version: 1,
			activeLayoutId: 'entryway',
			layouts: [
				{
					id: 'entryway',
					name: 'Entryway',
					mode: 'custom',
					customLayout: 'four',
					rows: 1,
					columns: 2,
					slots: [
						{ cameraId: 'front-door', stream: 'main' },
						{ cameraId: 'front-door', stream: 'sub' },
						{ cameraId: 'garage', stream: 'main' }
					]
				}
			]
		});

		expect(slotsForLayout(state.layouts[0])).toEqual([
			{ cameraId: 'front-door', stream: 'main' },
			null,
			{ cameraId: 'garage', stream: 'main' },
			null
		]);
	});

	it('persists an ordered Dynamic camera list', () => {
		const state = normalizePeekLayoutState({
			version: 1,
			activeLayoutId: 'patrol',
			layouts: [
				{
					id: 'patrol',
					name: 'Patrol',
					mode: 'dynamic',
					customLayout: 'grid',
					rows: 3,
					columns: 4,
					slots: [
						{ cameraId: 'garage', stream: 'sub' },
						{ cameraId: 'front-door', stream: 'main' },
						{ cameraId: 'garage', stream: 'main' }
					]
				}
			]
		});
		const values = new Map<string, string>();
		const storage = {
			getItem: (key: string) => values.get(key) ?? null,
			setItem: (key: string, value: string) => values.set(key, value)
		};

		expect(state.layouts[0].dynamicSlots).toEqual([
			{ cameraId: 'garage', stream: 'sub' },
			{ cameraId: 'front-door', stream: 'main' }
		]);
		savePeekLayoutState(storage, state);
		expect(loadPeekLayoutState(storage)).toEqual(state);
	});

	it('migrates the two-large-eight mosaic to the named ten-camera preset', () => {
		const state = normalizePeekLayoutState({
			version: 1,
			activeLayoutId: 'driveway',
			layouts: [
				{
					id: 'driveway',
					name: 'Driveway',
					mode: 'custom',
					customLayout: 'mosaic',
					rows: 2,
					columns: 2,
					slots: Array.from({ length: 10 }, (_, index) => ({
						cameraId: `camera-${index}`,
						stream: 'sub'
					}))
				}
			]
		});
		const layout = state.layouts[0];

		expect(layout.customLayout).toBe('ten');
		expect([layout.rows, layout.columns, slotCountForLayout(layout)]).toEqual([4, 4, 10]);
		expect(layoutSlotPlacement(layout, 0)).toEqual({
			column: 1,
			row: 1,
			columnSpan: 2,
			rowSpan: 2
		});
		expect(layoutSlotPlacement(layout, 1)).toEqual({
			column: 1,
			row: 3,
			columnSpan: 2,
			rowSpan: 2
		});
		expect(layoutSlotPlacement(layout, 9)).toEqual({
			column: 4,
			row: 4,
			columnSpan: 1,
			rowSpan: 1
		});
	});

	it('offers fixed film-grid variants for the same camera count', () => {
		expect(PEEK_LAYOUT_PRESETS.map((preset) => preset.id)).toEqual([
			'one',
			'two',
			'three',
			'four',
			'fiveFocus',
			'sixGrid',
			'six',
			'sixFocusRight',
			'seven',
			'sevenFocusRight',
			'eightGrid',
			'eightMosaic',
			'nine',
			'nineFocus',
			'nineFocusRight',
			'nineFocusBottom',
			'ten',
			'tenFocusRight',
			'tenGrid'
		]);
		expect(
			PEEK_LAYOUT_PRESETS.filter((preset) => preset.cameraCount === 10).map(
				(preset) => preset.label
			)
		).toEqual(['10 Focus Left', '10 Focus Right', '10 Grid']);
		expect(
			PEEK_LAYOUT_PRESETS.filter((preset) => preset.cameraCount === 9).map((preset) => preset.label)
		).toEqual(['9 Grid', '9 Focus Left', '9 Focus Right', '9 Focus Bottom']);
		expect(PEEK_LAYOUT_PICKER_PRESETS.map((preset) => preset.id)).not.toContain('four');
		expect(PEEK_LAYOUT_PICKER_PRESETS.map((preset) => preset.id)).not.toContain('nine');
		expect(
			PEEK_LAYOUT_PICKER_PRESETS.filter((preset) => preset.cameraCount === 9).map(
				(preset) => preset.label
			)
		).toEqual(['9 Focus Left', '9 Focus Right', '9 Focus Bottom']);
	});

	it('migrates legacy placement overrides to the selected film-grid template', () => {
		const state = normalizePeekLayoutState({
			version: 1,
			activeLayoutId: 'driveway',
			layouts: [
				{
					id: 'driveway',
					name: 'Driveway',
					mode: 'custom',
					customLayout: 'ten',
					rows: 4,
					columns: 4,
					slots: [
						{
							cameraId: 'front-door',
							stream: 'main',
							placement: { column: 3, row: 4, columnSpan: 1, rowSpan: 1 }
						}
					]
				}
			]
		});
		const layout = state.layouts[0];

		expect(layout.slots[0]).toEqual({ cameraId: 'front-door', stream: 'main' });
		expect(layoutSlotPlacement(layout, 0)).toEqual({
			column: 1,
			row: 1,
			columnSpan: 2,
			rowSpan: 2
		});
	});

	it('round-trips a valid state through browser storage', () => {
		const values = new Map<string, string>();
		const storage = {
			getItem: (key: string) => values.get(key) ?? null,
			setItem: (key: string, value: string) => values.set(key, value)
		};
		const state = createDefaultPeekLayoutState();

		savePeekLayoutState(storage, state);

		expect(values.has(PEEK_LAYOUT_STORAGE_KEY)).toBe(true);
		expect(loadPeekLayoutState(storage)).toEqual(state);
	});
});
