export const peekLayoutColumns = 12;
export const peekLayoutRows = 12;

export const peekLayoutPresets = ['2x2', '1+3', '1+5', '3x3', '4x4', '8-up'] as const;

export type PeekLayoutPreset = (typeof peekLayoutPresets)[number];

export type PeekLayoutItem = {
	cameraId: string;
	column: number;
	row: number;
	columnSpan: number;
	rowSpan: number;
	pinned: boolean;
};

export type PeekLayoutDraft = {
	preset: PeekLayoutPreset | 'custom';
	activityFocus: boolean;
	items: readonly PeekLayoutItem[];
};

type LayoutSlot = Omit<PeekLayoutItem, 'cameraId' | 'pinned'>;

const onePlusThreeSlots: readonly LayoutSlot[] = [
	{ column: 1, row: 1, columnSpan: 8, rowSpan: 12 },
	{ column: 9, row: 1, columnSpan: 4, rowSpan: 4 },
	{ column: 9, row: 9, columnSpan: 4, rowSpan: 4 },
	{ column: 9, row: 5, columnSpan: 4, rowSpan: 4 }
];

function regularSlots(columns: number, rows: number): LayoutSlot[] {
	const columnSpan = peekLayoutColumns / columns;
	const rowSpan = peekLayoutRows / rows;
	return Array.from({ length: columns * rows }, (_, index) => ({
		column: (index % columns) * columnSpan + 1,
		row: Math.floor(index / columns) * rowSpan + 1,
		columnSpan,
		rowSpan
	}));
}

function presetSlots(preset: PeekLayoutPreset): readonly LayoutSlot[] {
	switch (preset) {
		case '1+3':
			return onePlusThreeSlots;
		case '1+5':
			return [
				{ column: 1, row: 1, columnSpan: 8, rowSpan: 12 },
				...Array.from({ length: 5 }, (_, index) => ({
					column: 9,
					row: index * 2 + 1,
					columnSpan: 4,
					rowSpan: 2
				}))
			];
		case '2x2':
			return regularSlots(2, 2);
		case '3x3':
			return regularSlots(3, 3);
		case '4x4':
			return regularSlots(4, 4);
		case '8-up':
			return regularSlots(4, 2);
	}
}

function overlaps(left: LayoutSlot, right: LayoutSlot): boolean {
	return (
		left.column < right.column + right.columnSpan &&
		left.column + left.columnSpan > right.column &&
		left.row < right.row + right.rowSpan &&
		left.row + left.rowSpan > right.row
	);
}

function clamp(value: number, minimum: number, maximum: number): number {
	return Math.min(maximum, Math.max(minimum, value));
}

export function createPeekLayoutDraft(
	cameraIds: readonly string[],
	preset: PeekLayoutPreset = '1+3'
): PeekLayoutDraft {
	const slots = presetSlots(preset);
	return {
		preset,
		activityFocus: true,
		items: cameraIds.slice(0, slots.length).map((cameraId, index) => ({
			cameraId,
			...slots[index],
			pinned: index === 0
		}))
	};
}

export function applyPeekLayoutPreset(
	draft: PeekLayoutDraft,
	cameraIds: readonly string[],
	preset: PeekLayoutPreset
): PeekLayoutDraft {
	const pinnedCameraIds = new Set(
		draft.items.filter((item) => item.pinned).map((item) => item.cameraId)
	);
	const next = createPeekLayoutDraft(cameraIds, preset);
	return {
		...next,
		activityFocus: draft.activityFocus,
		items: next.items.map((item) => ({
			...item,
			pinned: pinnedCameraIds.has(item.cameraId)
		}))
	};
}

export function movePeekLayoutItem(
	draft: PeekLayoutDraft,
	cameraId: string,
	column: number,
	row: number
): PeekLayoutDraft {
	const item = draft.items.find((candidate) => candidate.cameraId === cameraId);
	if (item === undefined) return draft;

	const movedItem = {
		...item,
		column: clamp(Math.round(column), 1, peekLayoutColumns - item.columnSpan + 1),
		row: clamp(Math.round(row), 1, peekLayoutRows - item.rowSpan + 1)
	};
	const collides = draft.items.some(
		(candidate) => candidate.cameraId !== cameraId && overlaps(movedItem, candidate)
	);
	if (collides || (movedItem.column === item.column && movedItem.row === item.row)) return draft;

	return {
		...draft,
		preset: 'custom',
		items: draft.items.map((candidate) => (candidate.cameraId === cameraId ? movedItem : candidate))
	};
}

export function resizePeekLayoutItem(
	draft: PeekLayoutDraft,
	cameraId: string,
	columnSpan: number,
	rowSpan: number
): PeekLayoutDraft {
	const item = draft.items.find((candidate) => candidate.cameraId === cameraId);
	if (item === undefined) return draft;

	const resizedItem = {
		...item,
		columnSpan: clamp(Math.round(columnSpan), 2, peekLayoutColumns - item.column + 1),
		rowSpan: clamp(Math.round(rowSpan), 2, peekLayoutRows - item.row + 1)
	};
	const collides = draft.items.some(
		(candidate) => candidate.cameraId !== cameraId && overlaps(resizedItem, candidate)
	);
	if (
		collides ||
		(resizedItem.columnSpan === item.columnSpan && resizedItem.rowSpan === item.rowSpan)
	) {
		return draft;
	}

	return {
		...draft,
		preset: 'custom',
		items: draft.items.map((candidate) =>
			candidate.cameraId === cameraId ? resizedItem : candidate
		)
	};
}

export function addPeekLayoutCamera(draft: PeekLayoutDraft, cameraId: string): PeekLayoutDraft {
	if (draft.items.some((item) => item.cameraId === cameraId)) return draft;

	for (const row of [1, 5, 9]) {
		for (const column of [1, 5, 9]) {
			const candidate = { column, row, columnSpan: 4, rowSpan: 4 };
			if (draft.items.every((item) => !overlaps(candidate, item))) {
				return {
					...draft,
					preset: 'custom',
					items: [...draft.items, { cameraId, ...candidate, pinned: false }]
				};
			}
		}
	}

	return draft;
}

export function setPeekLayoutPinned(
	draft: PeekLayoutDraft,
	cameraId: string,
	pinned: boolean
): PeekLayoutDraft {
	if (!draft.items.some((item) => item.cameraId === cameraId)) return draft;
	return {
		...draft,
		items: draft.items.map((item) => (item.cameraId === cameraId ? { ...item, pinned } : item))
	};
}

export function setPeekLayoutActivityFocus(
	draft: PeekLayoutDraft,
	activityFocus: boolean
): PeekLayoutDraft {
	return draft.activityFocus === activityFocus ? draft : { ...draft, activityFocus };
}
