export const PEEK_LAYOUT_STORAGE_KEY = 'keeppeek-peek-layouts-v1';

const MIN_GRID_DIMENSION = 1;
const MAX_GRID_DIMENSION = 6;
const MAX_LAYOUTS = 24;
const MAX_DYNAMIC_SLOTS = 64;

export type PeekLayoutMode = 'dynamic' | 'matrix' | 'custom';
export type PeekLayoutPresetId =
	| 'one'
	| 'two'
	| 'three'
	| 'four'
	| 'fiveFocus'
	| 'six'
	| 'sixFocusRight'
	| 'sixGrid'
	| 'seven'
	| 'sevenFocusRight'
	| 'eightGrid'
	| 'eightMosaic'
	| 'nine'
	| 'nineFocus'
	| 'nineFocusRight'
	| 'nineFocusBottom'
	| 'ten'
	| 'tenFocusRight'
	| 'tenGrid';
export type PeekCustomLayout = PeekLayoutPresetId | 'grid';

export type PeekLayoutSlot = {
	cameraId: string;
	stream: 'main' | 'sub';
};

export type PeekLayout = {
	id: string;
	name: string;
	mode: PeekLayoutMode;
	customLayout: PeekCustomLayout;
	columns: number;
	rows: number;
	slots: Array<PeekLayoutSlot | null>;
	dynamicSlots?: PeekLayoutSlot[];
};

export type PeekLayoutPlacement = {
	column: number;
	row: number;
	columnSpan: number;
	rowSpan: number;
};

export type PeekLayoutPreset = {
	id: PeekLayoutPresetId;
	label: string;
	showInPicker?: boolean;
	cameraCount: number;
	rows: number;
	columns: number;
	placements: PeekLayoutPlacement[];
};

function gridPlacements(rows: number, columns: number): PeekLayoutPlacement[] {
	return Array.from({ length: rows * columns }, (_, index) => ({
		column: (index % columns) + 1,
		row: Math.floor(index / columns) + 1,
		columnSpan: 1,
		rowSpan: 1
	}));
}

export const PEEK_LAYOUT_PRESETS: PeekLayoutPreset[] = [
	{
		id: 'one',
		label: '1 Camera',
		cameraCount: 1,
		rows: 1,
		columns: 1,
		placements: [{ column: 1, row: 1, columnSpan: 1, rowSpan: 1 }]
	},
	{
		id: 'two',
		label: '2 Grid',
		cameraCount: 2,
		rows: 1,
		columns: 2,
		placements: gridPlacements(1, 2)
	},
	{
		id: 'three',
		label: '3 Grid',
		cameraCount: 3,
		rows: 1,
		columns: 3,
		placements: gridPlacements(1, 3)
	},
	{
		id: 'four',
		label: '4 Grid',
		showInPicker: false,
		cameraCount: 4,
		rows: 2,
		columns: 2,
		placements: [
			{ column: 1, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 2, columnSpan: 1, rowSpan: 1 }
		]
	},
	{
		id: 'fiveFocus',
		label: '5 Focus',
		cameraCount: 5,
		rows: 2,
		columns: 4,
		placements: [
			{ column: 1, row: 1, columnSpan: 2, rowSpan: 2 },
			{ column: 3, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 2, columnSpan: 1, rowSpan: 1 }
		]
	},
	{
		id: 'sixGrid',
		label: '6 Grid',
		cameraCount: 6,
		rows: 2,
		columns: 3,
		placements: [
			{ column: 1, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 2, columnSpan: 1, rowSpan: 1 }
		]
	},
	{
		id: 'six',
		label: '6 Focus Left',
		cameraCount: 6,
		rows: 2,
		columns: 4,
		placements: [
			{ column: 1, row: 1, columnSpan: 2, rowSpan: 1 },
			{ column: 1, row: 2, columnSpan: 2, rowSpan: 1 },
			{ column: 3, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 2, columnSpan: 1, rowSpan: 1 }
		]
	},
	{
		id: 'sixFocusRight',
		label: '6 Focus Right',
		cameraCount: 6,
		rows: 2,
		columns: 4,
		placements: [
			{ column: 1, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 1, columnSpan: 2, rowSpan: 1 },
			{ column: 3, row: 2, columnSpan: 2, rowSpan: 1 }
		]
	},
	{
		id: 'seven',
		label: '7 Focus Left',
		cameraCount: 7,
		rows: 3,
		columns: 4,
		placements: [
			{ column: 1, row: 1, columnSpan: 2, rowSpan: 2 },
			{ column: 3, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 3, columnSpan: 1, rowSpan: 1 }
		]
	},
	{
		id: 'sevenFocusRight',
		label: '7 Focus Right',
		cameraCount: 7,
		rows: 3,
		columns: 4,
		placements: [
			{ column: 3, row: 1, columnSpan: 2, rowSpan: 2 },
			{ column: 1, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 3, columnSpan: 1, rowSpan: 1 }
		]
	},
	{
		id: 'eightGrid',
		label: '8 Grid',
		cameraCount: 8,
		rows: 2,
		columns: 4,
		placements: gridPlacements(2, 4)
	},
	{
		id: 'eightMosaic',
		label: '8 Mosaic',
		cameraCount: 8,
		rows: 3,
		columns: 4,
		placements: [
			{ column: 1, row: 1, columnSpan: 2, rowSpan: 1 },
			{ column: 3, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 2, columnSpan: 2, rowSpan: 1 },
			{ column: 3, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 3, columnSpan: 2, rowSpan: 1 },
			{ column: 3, row: 3, columnSpan: 2, rowSpan: 1 }
		]
	},
	{
		id: 'nine',
		label: '9 Grid',
		showInPicker: false,
		cameraCount: 9,
		rows: 3,
		columns: 3,
		placements: [
			{ column: 1, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 3, columnSpan: 1, rowSpan: 1 }
		]
	},
	{
		id: 'nineFocus',
		label: '9 Focus Left',
		cameraCount: 9,
		rows: 3,
		columns: 4,
		placements: [
			{ column: 1, row: 1, columnSpan: 2, rowSpan: 2 },
			{ column: 3, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 3, columnSpan: 1, rowSpan: 1 }
		]
	},
	{
		id: 'nineFocusRight',
		label: '9 Focus Right',
		cameraCount: 9,
		rows: 3,
		columns: 4,
		placements: [
			{ column: 3, row: 1, columnSpan: 2, rowSpan: 2 },
			{ column: 1, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 3, columnSpan: 1, rowSpan: 1 }
		]
	},
	{
		id: 'nineFocusBottom',
		label: '9 Focus Bottom',
		cameraCount: 9,
		rows: 3,
		columns: 4,
		placements: [
			{ column: 1, row: 2, columnSpan: 2, rowSpan: 2 },
			{ column: 3, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 1, columnSpan: 1, rowSpan: 1 }
		]
	},
	{
		id: 'ten',
		label: '10 Focus Left',
		cameraCount: 10,
		rows: 4,
		columns: 4,
		placements: [
			{ column: 1, row: 1, columnSpan: 2, rowSpan: 2 },
			{ column: 1, row: 3, columnSpan: 2, rowSpan: 2 },
			{ column: 3, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 4, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 4, columnSpan: 1, rowSpan: 1 }
		]
	},
	{
		id: 'tenFocusRight',
		label: '10 Focus Right',
		cameraCount: 10,
		rows: 4,
		columns: 4,
		placements: [
			{ column: 3, row: 1, columnSpan: 2, rowSpan: 2 },
			{ column: 3, row: 3, columnSpan: 2, rowSpan: 2 },
			{ column: 1, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 3, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 4, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 4, columnSpan: 1, rowSpan: 1 }
		]
	},
	{
		id: 'tenGrid',
		label: '10 Grid',
		cameraCount: 10,
		rows: 2,
		columns: 5,
		placements: [
			{ column: 1, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 5, row: 1, columnSpan: 1, rowSpan: 1 },
			{ column: 1, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 2, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 3, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 4, row: 2, columnSpan: 1, rowSpan: 1 },
			{ column: 5, row: 2, columnSpan: 1, rowSpan: 1 }
		]
	}
];

export const PEEK_LAYOUT_PICKER_PRESETS = PEEK_LAYOUT_PRESETS.filter(
	(preset) => preset.showInPicker !== false
);

export const DEFAULT_PEEK_CUSTOM_LAYOUT: PeekLayoutPresetId = 'ten';

export type PeekLayoutState = {
	version: 1;
	activeLayoutId: string;
	layouts: PeekLayout[];
};

type StorageLike = Pick<Storage, 'getItem' | 'setItem'>;

export function createDefaultPeekLayoutState(): PeekLayoutState {
	return {
		version: 1,
		activeLayoutId: 'dynamic',
		layouts: [
			{
				id: 'dynamic',
				name: 'Dynamic',
				mode: 'dynamic',
				customLayout: 'grid',
				columns: 4,
				rows: 3,
				slots: [],
				dynamicSlots: []
			}
		]
	};
}

export function createPeekLayout(id: string, name = 'New view'): PeekLayout {
	const preset = peekLayoutPreset(DEFAULT_PEEK_CUSTOM_LAYOUT)!;
	return {
		id,
		name,
		mode: 'custom',
		customLayout: preset.id,
		columns: preset.columns,
		rows: preset.rows,
		slots: Array.from({ length: preset.placements.length }, () => null),
		dynamicSlots: []
	};
}

export function createPeekLayoutId(): string {
	if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
		return crypto.randomUUID();
	}
	return `view-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

export function clonePeekLayoutState(state: PeekLayoutState): PeekLayoutState {
	return {
		...state,
		layouts: state.layouts.map((layout) => ({
			...layout,
			slots: layout.slots.map((slot) => (slot ? { ...slot } : null)),
			dynamicSlots: layout.dynamicSlots?.map((slot) => ({ ...slot }))
		}))
	};
}

export function slotsForLayout(layout: PeekLayout): Array<PeekLayoutSlot | null> {
	const slotCount = slotCountForLayout(layout);
	const slots = layout.slots.slice(0, slotCount);
	while (slots.length < slotCount) slots.push(null);
	return slots;
}

export function slotCountForLayout(layout: PeekLayout): number {
	return slotCountFor(layout.mode, layout.customLayout, layout.rows, layout.columns);
}

export function layoutSlotPlacement(layout: PeekLayout, index: number): PeekLayoutPlacement {
	const preset = layout.mode === 'custom' ? peekLayoutPreset(layout.customLayout) : null;
	if (preset?.placements[index]) {
		return preset.placements[index];
	}

	return {
		column: (index % layout.columns) + 1,
		row: Math.floor(index / layout.columns) + 1,
		columnSpan: 1,
		rowSpan: 1
	};
}

export function peekLayoutPreset(layout: PeekCustomLayout): PeekLayoutPreset | null {
	return PEEK_LAYOUT_PRESETS.find((preset) => preset.id === layout) ?? null;
}

export function orderedDynamicCameraIds(
	layout: Pick<PeekLayout, 'dynamicSlots'>,
	cameraIds: Iterable<string>
): string[] {
	const availableIds = [...cameraIds];
	const remainingIds = new Set(availableIds);
	const orderedIds: string[] = [];
	for (const slot of layout.dynamicSlots ?? []) {
		if (!remainingIds.delete(slot.cameraId)) continue;
		orderedIds.push(slot.cameraId);
	}
	return [...orderedIds, ...availableIds.filter((cameraId) => remainingIds.has(cameraId))];
}

export function normalizePeekLayoutState(value: unknown): PeekLayoutState {
	if (!isRecord(value) || value.version !== 1 || !Array.isArray(value.layouts)) {
		return createDefaultPeekLayoutState();
	}

	const seenIds = new Set<string>();
	const layouts = value.layouts
		.slice(0, MAX_LAYOUTS)
		.map(normalizeLayout)
		.filter((layout): layout is PeekLayout => layout !== null)
		.filter((layout) => {
			if (seenIds.has(layout.id)) return false;
			seenIds.add(layout.id);
			return true;
		});

	if (layouts.length === 0) return createDefaultPeekLayoutState();
	const activeLayoutId =
		typeof value.activeLayoutId === 'string' && seenIds.has(value.activeLayoutId)
			? value.activeLayoutId
			: layouts[0].id;

	return { version: 1, activeLayoutId, layouts };
}

export function loadPeekLayoutState(storage: Pick<StorageLike, 'getItem'> | null): PeekLayoutState {
	if (!storage) return createDefaultPeekLayoutState();
	try {
		const serialized = storage.getItem(PEEK_LAYOUT_STORAGE_KEY);
		return serialized
			? normalizePeekLayoutState(JSON.parse(serialized))
			: createDefaultPeekLayoutState();
	} catch {
		return createDefaultPeekLayoutState();
	}
}

export function savePeekLayoutState(
	storage: Pick<StorageLike, 'setItem'> | null,
	state: PeekLayoutState
): void {
	if (!storage) return;
	try {
		storage.setItem(PEEK_LAYOUT_STORAGE_KEY, JSON.stringify(normalizePeekLayoutState(state)));
	} catch {
		// Layout changes remain active for this page when browser storage is unavailable.
	}
}

function normalizeLayout(value: unknown): PeekLayout | null {
	if (!isRecord(value) || typeof value.id !== 'string' || !value.id.trim()) return null;
	if (!isPeekLayoutMode(value.mode)) return null;

	const normalizedCustomLayout = normalizeCustomLayout(value.customLayout);
	const customLayout =
		value.mode === 'custom' && normalizedCustomLayout === 'grid' ? 'nine' : normalizedCustomLayout;
	const preset = peekLayoutPreset(customLayout);
	const rows = preset ? preset.rows : normalizeDimension(value.rows, 2);
	const columns = preset ? preset.columns : normalizeDimension(value.columns, 2);
	const slots = preset ? normalizeSlots(value.slots, preset.placements.length) : [];
	const dynamicSlots = normalizeDynamicSlots(
		value.dynamicSlots ?? (value.mode === 'dynamic' ? value.slots : [])
	);
	return {
		id: value.id.trim(),
		name:
			typeof value.name === 'string' && value.name.trim()
				? value.name.trim().slice(0, 80)
				: 'Untitled',
		mode: value.mode,
		customLayout,
		rows,
		columns,
		slots,
		dynamicSlots
	};
}

function normalizeDimension(value: unknown, fallback: number): number {
	if (typeof value !== 'number' || !Number.isInteger(value)) return fallback;
	return Math.min(MAX_GRID_DIMENSION, Math.max(MIN_GRID_DIMENSION, value));
}

function normalizeSlots(value: unknown, slotCount: number): Array<PeekLayoutSlot | null> {
	const source = Array.isArray(value) ? value : [];
	const cameraIds = new Set<string>();
	return Array.from({ length: slotCount }, (_, index) => {
		const slot = source[index];
		if (!isRecord(slot) || typeof slot.cameraId !== 'string' || !slot.cameraId.trim()) return null;
		if (slot.stream !== 'main' && slot.stream !== 'sub') return null;
		const cameraId = slot.cameraId.trim();
		if (cameraIds.has(cameraId)) return null;
		cameraIds.add(cameraId);
		return { cameraId, stream: slot.stream };
	});
}

function normalizeDynamicSlots(value: unknown): PeekLayoutSlot[] {
	const source = Array.isArray(value) ? value.slice(0, MAX_DYNAMIC_SLOTS) : [];
	const cameraIds = new Set<string>();
	const slots: PeekLayoutSlot[] = [];
	for (const slot of source) {
		if (!isRecord(slot) || typeof slot.cameraId !== 'string' || !slot.cameraId.trim()) continue;
		if (slot.stream !== 'main' && slot.stream !== 'sub') continue;
		const cameraId = slot.cameraId.trim();
		if (cameraIds.has(cameraId)) continue;
		cameraIds.add(cameraId);
		slots.push({ cameraId, stream: slot.stream });
	}
	return slots;
}

function slotCountFor(
	mode: PeekLayoutMode,
	customLayout: PeekCustomLayout,
	rows: number,
	columns: number
): number {
	return mode === 'custom'
		? (peekLayoutPreset(customLayout)?.placements.length ?? rows * columns)
		: rows * columns;
}

function normalizeCustomLayout(value: unknown): PeekCustomLayout {
	if (value === 'mosaic') return 'ten';
	if (value === 'grid') return 'grid';
	return PEEK_LAYOUT_PRESETS.find((preset) => preset.id === value)?.id ?? 'grid';
}

function isPeekLayoutMode(value: unknown): value is PeekLayoutMode {
	return value === 'dynamic' || value === 'matrix' || value === 'custom';
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null;
}
