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

export type PeekLayoutScope = 'private' | 'shared';

export type PeekLayoutAudience = {
	everyone: boolean;
	credentialIds: readonly string[];
};

export type PeekLayout = {
	id: string;
	name: string;
	scope: PeekLayoutScope;
	ownerId: string;
	audience: PeekLayoutAudience;
	activityFocus: boolean;
	items: readonly PeekLayoutItem[];
};

export type PeekLayoutRegistry = {
	schemaVersion: 1;
	revision: string;
	activeLayoutId: string;
	layouts: readonly PeekLayout[];
};

export type CreatePeekLayout = {
	id: string;
	name: string;
	ownerId: string;
	scope?: PeekLayoutScope;
	audience?: PeekLayoutAudience;
	draft: PeekLayoutDraft;
};

export type DuplicatePeekLayout = {
	id: string;
	name: string;
	ownerId: string;
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

function requireLayout(registry: PeekLayoutRegistry, layoutId: string): PeekLayout {
	const layout = registry.layouts.find((candidate) => candidate.id === layoutId);
	if (layout === undefined) throw new Error('Layout does not exist.');
	return layout;
}

function validatedIdentity(registry: PeekLayoutRegistry, id: string, name: string) {
	const normalizedId = id.trim();
	const normalizedName = name.trim();
	if (normalizedId.length === 0 || normalizedId.length > 128) {
		throw new Error('Layout ID is invalid.');
	}
	if (registry.layouts.some((layout) => layout.id === normalizedId)) {
		throw new Error('Layout ID already exists.');
	}
	if (normalizedName.length === 0 || [...normalizedName].length > 80) {
		throw new Error('Layout name is invalid.');
	}
	return { id: normalizedId, name: normalizedName };
}

function savedLayout(
	identity: { id: string; name: string },
	ownerId: string,
	draft: PeekLayoutDraft,
	scope: PeekLayoutScope = 'private',
	audience?: PeekLayoutAudience
): PeekLayout {
	const normalizedOwnerId = ownerId.trim();
	if (normalizedOwnerId.length === 0) throw new Error('Layout owner is invalid.');
	return {
		...identity,
		scope,
		ownerId: normalizedOwnerId,
		audience: canonicalAudience(
			audience ?? {
				everyone: scope === 'shared',
				credentialIds: scope === 'private' ? [normalizedOwnerId] : []
			}
		),
		activityFocus: draft.activityFocus,
		items: draft.items.map((item) => ({ ...item }))
	};
}

function canonicalAudience(audience: PeekLayoutAudience): PeekLayoutAudience {
	if (audience.everyone) return { everyone: true, credentialIds: [] };
	const credentialIds = audience.credentialIds.map((id) => id.trim());
	if (credentialIds.some((id) => id.length === 0 || id.length > 128)) {
		throw new Error('Dashboard viewer identity is invalid.');
	}
	return {
		everyone: false,
		credentialIds: [...new Set(credentialIds)].toSorted()
	};
}

export function createPeekLayout(
	registry: PeekLayoutRegistry,
	input: CreatePeekLayout
): PeekLayoutRegistry {
	const identity = validatedIdentity(registry, input.id, input.name);
	return {
		...registry,
		activeLayoutId: identity.id,
		layouts: [
			...registry.layouts,
			savedLayout(identity, input.ownerId, input.draft, input.scope, input.audience)
		]
	};
}

export function renamePeekLayout(
	registry: PeekLayoutRegistry,
	layoutId: string,
	name: string
): PeekLayoutRegistry {
	const layout = requireLayout(registry, layoutId);
	const normalizedName = name.trim();
	if (normalizedName.length === 0 || [...normalizedName].length > 80) {
		throw new Error('Layout name is invalid.');
	}
	if (layout.name === normalizedName) return registry;
	return {
		...registry,
		layouts: registry.layouts.map((candidate) =>
			candidate.id === layoutId ? { ...candidate, name: normalizedName } : candidate
		)
	};
}

export function duplicatePeekLayout(
	registry: PeekLayoutRegistry,
	layoutId: string,
	input: DuplicatePeekLayout
): PeekLayoutRegistry {
	const source = requireLayout(registry, layoutId);
	const identity = validatedIdentity(registry, input.id, input.name);
	const duplicate = savedLayout(
		identity,
		input.ownerId,
		{
			preset: 'custom',
			activityFocus: source.activityFocus,
			items: source.items
		},
		source.scope,
		source.audience
	);
	return {
		...registry,
		activeLayoutId: duplicate.id,
		layouts: [...registry.layouts, duplicate]
	};
}

export function selectPeekLayout(
	registry: PeekLayoutRegistry,
	layoutId: string
): PeekLayoutRegistry {
	requireLayout(registry, layoutId);
	return registry.activeLayoutId === layoutId
		? registry
		: { ...registry, activeLayoutId: layoutId };
}

export function deletePeekLayout(
	registry: PeekLayoutRegistry,
	layoutId: string
): PeekLayoutRegistry {
	requireLayout(registry, layoutId);
	if (registry.layouts.length === 1) throw new Error('The final layout cannot be deleted.');
	const layouts = registry.layouts.filter((layout) => layout.id !== layoutId);
	return {
		...registry,
		activeLayoutId:
			registry.activeLayoutId === layoutId ? (layouts[0]?.id ?? '') : registry.activeLayoutId,
		layouts
	};
}

export function updatePeekLayout(
	registry: PeekLayoutRegistry,
	layoutId: string,
	draft: PeekLayoutDraft
): PeekLayoutRegistry {
	requireLayout(registry, layoutId);
	return {
		...registry,
		layouts: registry.layouts.map((layout) =>
			layout.id === layoutId
				? {
						...layout,
						activityFocus: draft.activityFocus,
						items: draft.items.map((item) => ({ ...item }))
					}
				: layout
		)
	};
}

export function updatePeekLayoutAudience(
	registry: PeekLayoutRegistry,
	layoutId: string,
	audience: PeekLayoutAudience
): PeekLayoutRegistry {
	requireLayout(registry, layoutId);
	const canonical = canonicalAudience(audience);
	return {
		...registry,
		layouts: registry.layouts.map((layout) =>
			layout.id === layoutId ? { ...layout, audience: canonical } : layout
		)
	};
}

export function peekLayoutDraft(layout: PeekLayout): PeekLayoutDraft {
	return {
		preset: 'custom',
		activityFocus: layout.activityFocus,
		items: layout.items.map((item) => ({ ...item }))
	};
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
	if (draft.items.length >= 64 || draft.items.some((item) => item.cameraId === cameraId)) {
		return draft;
	}

	for (const size of [4, 3, 2, 1]) {
		for (let row = 1; row <= peekLayoutRows - size + 1; row += 1) {
			for (let column = 1; column <= peekLayoutColumns - size + 1; column += 1) {
				const candidate = { column, row, columnSpan: size, rowSpan: size };
				if (draft.items.every((item) => !overlaps(candidate, item))) {
					return {
						...draft,
						preset: 'custom',
						items: [...draft.items, { cameraId, ...candidate, pinned: false }]
					};
				}
			}
		}
	}

	return draft;
}

export function removePeekLayoutCamera(draft: PeekLayoutDraft, cameraId: string): PeekLayoutDraft {
	if (!draft.items.some((item) => item.cameraId === cameraId)) return draft;
	return {
		...draft,
		preset: 'custom',
		items: draft.items.filter((item) => item.cameraId !== cameraId)
	};
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
