import type {
	PeekLayout,
	PeekLayoutAudience,
	PeekLayoutItem,
	PeekLayoutRegistry,
	PeekLayoutScope
} from './peek-layout';

const maxDocumentBytes = 256 * 1_024;
const maxLayouts = 32;
const maxTiles = 64;

type UnknownRecord = Record<string, unknown>;

export type PeekLayoutImportPreview = {
	schemaVersion: 1;
	activeLayoutId: string;
	layouts: readonly PeekLayout[];
	missingCameraIds: readonly string[];
	conflictingLayoutIds: readonly string[];
	unsupportedFields: readonly string[];
};

export type PeekLayoutImportOptions = {
	ownerId: string;
	targetScope?: PeekLayoutScope;
	targetAudience?: PeekLayoutAudience;
	availableCameraIds: readonly string[];
	missingCameraMappings: Readonly<Record<string, string | null>>;
	conflictResolution: 'duplicate' | 'reject' | 'replace';
	preserveOwnership?: boolean;
	idFactory?: () => string;
};

export function exportPeekLayoutRegistry(registry: PeekLayoutRegistry, layoutId?: string): string {
	const layouts = layoutId
		? registry.layouts.filter((layout) => layout.id === layoutId)
		: registry.layouts;
	if (layouts.length === 0) throw new Error('Layout does not exist.');
	const activeLayoutId = layoutId ?? registry.activeLayoutId;
	return `${JSON.stringify(
		{
			schema_version: 1,
			active_layout_id: activeLayoutId,
			layouts: layouts.map(layoutToWire)
		},
		null,
		2
	)}\n`;
}

export function previewPeekLayoutImport(
	source: string,
	current: PeekLayoutRegistry,
	availableCameraIds: readonly string[]
): PeekLayoutImportPreview {
	if (new TextEncoder().encode(source).byteLength > maxDocumentBytes) {
		throw new Error('Layout import is too large.');
	}
	let parsed: unknown;
	try {
		parsed = JSON.parse(source);
	} catch {
		throw new Error('Layout import is not valid JSON.');
	}
	const unsupportedFields: string[] = [];
	const document = record(parsed, 'layout import');
	collectUnsupported(
		document,
		['schema_version', 'active_layout_id', 'layouts'],
		'',
		unsupportedFields
	);
	if (wholeNumber(document.schema_version, 'layout schema version') !== 1) {
		throw new Error('Layout import schema version is unsupported.');
	}
	const activeLayoutId = boundedText(document.active_layout_id, 'active layout ID', 128);
	const values = array(document.layouts, 'layouts');
	if (values.length === 0 || values.length > maxLayouts) {
		throw new Error('Layout import has an invalid layout count.');
	}
	const layouts = values.map((value, index) =>
		layoutFromWire(value, `layouts[${index}]`, unsupportedFields)
	);
	const layoutIds = new Set(layouts.map((layout) => layout.id));
	if (layoutIds.size !== layouts.length || !layoutIds.has(activeLayoutId)) {
		throw new Error('Layout import has invalid layout identities.');
	}
	const available = new Set(availableCameraIds);
	const missingCameraIds = [
		...new Set(
			layouts.flatMap((layout) =>
				layout.items.filter((item) => !available.has(item.cameraId)).map((item) => item.cameraId)
			)
		)
	].toSorted();
	const currentIds = new Set(current.layouts.map((layout) => layout.id));
	const conflictingLayoutIds = layouts
		.map((layout) => layout.id)
		.filter((id) => currentIds.has(id));
	return {
		schemaVersion: 1,
		activeLayoutId,
		layouts,
		missingCameraIds,
		conflictingLayoutIds,
		unsupportedFields
	};
}

export function applyPeekLayoutImport(
	current: PeekLayoutRegistry,
	preview: PeekLayoutImportPreview,
	options: PeekLayoutImportOptions
): PeekLayoutRegistry {
	if (preview.unsupportedFields.length > 0) {
		throw new Error('Remove unsupported fields before importing.');
	}
	if (preview.conflictingLayoutIds.length > 0 && options.conflictResolution === 'reject') {
		throw new Error('Choose how to resolve conflicting layout IDs.');
	}
	const available = new Set(options.availableCameraIds);
	for (const cameraId of preview.missingCameraIds) {
		if (!Object.hasOwn(options.missingCameraMappings, cameraId)) {
			throw new Error('Choose a mapping or omit every missing camera.');
		}
		const mapped = options.missingCameraMappings[cameraId];
		if (mapped !== null && !available.has(mapped)) {
			throw new Error('A missing camera mapping is not available.');
		}
	}

	const ownerId = options.ownerId.trim();
	if (!options.preserveOwnership && ownerId.length === 0) {
		throw new Error('Layout owner is invalid.');
	}
	const currentIds = new Set(current.layouts.map((layout) => layout.id));
	const usedIds = new Set([...currentIds, ...preview.layouts.map((layout) => layout.id)]);
	const importedIds = new Map<string, string>();
	const imported = preview.layouts.map((layout) => {
		let id = layout.id;
		if (currentIds.has(id) && options.conflictResolution === 'duplicate') {
			id = options.idFactory?.() ?? crypto.randomUUID();
			if (id.trim().length === 0 || usedIds.has(id)) {
				throw new Error('Duplicate layout ID is invalid.');
			}
		}
		usedIds.add(id);
		importedIds.set(layout.id, id);
		const items = layout.items.flatMap((item) => {
			if (available.has(item.cameraId)) return [{ ...item }];
			const mapped = options.missingCameraMappings[item.cameraId];
			return mapped === null ? [] : [{ ...item, cameraId: mapped }];
		});
		validateItems(items);
		const preserveSharedOwnership = options.preserveOwnership && layout.scope === 'shared';
		const targetScope = options.targetScope ?? (preserveSharedOwnership ? 'shared' : 'private');
		const targetOwnerId = preserveSharedOwnership ? layout.ownerId : ownerId;
		return {
			sourceId: layout.id,
			layout: {
				...layout,
				id,
				scope: targetScope,
				ownerId: targetOwnerId,
				audience:
					options.targetAudience ??
					(targetScope === 'shared'
						? layout.audience
						: { everyone: false, credentialIds: [targetOwnerId] }),
				items
			}
		};
	});

	let layouts = [...current.layouts];
	for (const importedLayout of imported) {
		const existingIndex = layouts.findIndex(
			(candidate) => candidate.id === importedLayout.sourceId
		);
		if (existingIndex >= 0 && options.conflictResolution === 'replace') {
			if (layouts[existingIndex]?.scope === 'shared' && importedLayout.layout.scope !== 'shared') {
				throw new Error('A private layout cannot replace a shared layout.');
			}
			layouts[existingIndex] = importedLayout.layout;
		} else {
			layouts.push(importedLayout.layout);
		}
	}
	if (layouts.length === 0 || layouts.length > maxLayouts) {
		throw new Error('Imported registry has an invalid layout count.');
	}
	return {
		...current,
		activeLayoutId: importedIds.get(preview.activeLayoutId) ?? preview.activeLayoutId,
		layouts
	};
}

function layoutToWire(layout: PeekLayout) {
	return {
		id: layout.id,
		name: layout.name,
		scope: layout.scope,
		owner_id: layout.ownerId,
		audience: {
			everyone: layout.audience.everyone,
			credential_ids: [...layout.audience.credentialIds]
		},
		activity_focus: layout.activityFocus,
		tiles: layout.items.map((item) => ({
			camera_id: item.cameraId,
			column: item.column,
			row: item.row,
			column_span: item.columnSpan,
			row_span: item.rowSpan,
			pinned: item.pinned
		}))
	};
}

function layoutFromWire(value: unknown, path: string, unsupportedFields: string[]): PeekLayout {
	const wire = record(value, 'layout');
	collectUnsupported(
		wire,
		['id', 'name', 'scope', 'owner_id', 'audience', 'activity_focus', 'tiles'],
		path,
		unsupportedFields
	);
	const tiles = array(wire.tiles, 'layout tiles');
	if (tiles.length > maxTiles) throw new Error('Layout import has too many tiles.');
	const items = tiles.map((tile, index) =>
		tileFromWire(tile, `${path}.tiles[${index}]`, unsupportedFields)
	);
	validateItems(items);
	const layoutScope = scope(wire.scope);
	const ownerId = boundedText(wire.owner_id, 'layout owner', 128);
	return {
		id: boundedText(wire.id, 'layout ID', 128),
		name: boundedText(wire.name, 'layout name', 80),
		scope: layoutScope,
		ownerId,
		audience: layoutAudience(wire.audience, layoutScope, ownerId),
		activityFocus: boolean(wire.activity_focus, 'layout activity focus'),
		items
	};
}

function layoutAudience(
	value: unknown,
	layoutScope: PeekLayoutScope,
	ownerId: string
): PeekLayoutAudience {
	if (value === undefined) {
		return layoutScope === 'shared'
			? { everyone: true, credentialIds: [] }
			: { everyone: false, credentialIds: [ownerId] };
	}
	const wire = record(value, 'layout audience');
	collectUnsupported(wire, ['everyone', 'credential_ids'], 'audience', []);
	const everyone = boolean(wire.everyone, 'layout audience');
	const credentialIds = array(wire.credential_ids, 'layout viewer identities').map((identity) =>
		boundedText(identity, 'layout viewer identity', 128)
	);
	if (
		(everyone && credentialIds.length > 0) ||
		new Set(credentialIds).size !== credentialIds.length
	) {
		throw new Error('Layout import has an invalid audience.');
	}
	return { everyone, credentialIds };
}

function tileFromWire(value: unknown, path: string, unsupportedFields: string[]): PeekLayoutItem {
	const wire = record(value, 'layout tile');
	collectUnsupported(
		wire,
		['camera_id', 'column', 'row', 'column_span', 'row_span', 'pinned'],
		path,
		unsupportedFields
	);
	return {
		cameraId: boundedText(wire.camera_id, 'camera ID', 256),
		column: wholeNumber(wire.column, 'tile column'),
		row: wholeNumber(wire.row, 'tile row'),
		columnSpan: wholeNumber(wire.column_span, 'tile column span'),
		rowSpan: wholeNumber(wire.row_span, 'tile row span'),
		pinned: boolean(wire.pinned, 'tile pin')
	};
}

function validateItems(items: readonly PeekLayoutItem[]): void {
	const cameraIds = new Set<string>();
	for (const [index, item] of items.entries()) {
		if (cameraIds.has(item.cameraId)) {
			throw new Error('Layout import contains a duplicate camera.');
		}
		cameraIds.add(item.cameraId);
		if (
			item.column < 1 ||
			item.row < 1 ||
			item.columnSpan < 1 ||
			item.rowSpan < 1 ||
			item.column + item.columnSpan > 13 ||
			item.row + item.rowSpan > 13
		) {
			throw new Error('Layout import has a tile outside the grid.');
		}
		if (items.slice(0, index).some((other) => overlaps(item, other))) {
			throw new Error('Layout import contains overlapping tiles.');
		}
	}
}

function collectUnsupported(
	value: UnknownRecord,
	allowed: readonly string[],
	path: string,
	result: string[]
): void {
	const allowedFields = new Set(allowed);
	for (const field of Object.keys(value)) {
		if (!allowedFields.has(field)) result.push(path ? `${path}.${field}` : field);
	}
}

function record(value: unknown, label: string): UnknownRecord {
	if (value === null || typeof value !== 'object' || Array.isArray(value)) {
		throw new Error(`Invalid ${label}.`);
	}
	return value as UnknownRecord;
}

function array(value: unknown, label: string): unknown[] {
	if (!Array.isArray(value)) throw new Error(`Invalid ${label}.`);
	return value;
}

function text(value: unknown, label: string): string {
	if (typeof value !== 'string' || value.trim().length === 0) {
		throw new Error(`Invalid ${label}.`);
	}
	return value;
}

function boundedText(value: unknown, label: string, maximum: number): string {
	const result = text(value, label);
	if ([...result].length > maximum) throw new Error(`Invalid ${label}.`);
	return result;
}

function boolean(value: unknown, label: string): boolean {
	if (typeof value !== 'boolean') throw new Error(`Invalid ${label}.`);
	return value;
}

function wholeNumber(value: unknown, label: string): number {
	if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
		throw new Error(`Invalid ${label}.`);
	}
	return value;
}

function scope(value: unknown): PeekLayoutScope {
	if (value !== 'private' && value !== 'shared') throw new Error('Invalid layout scope.');
	return value;
}

function overlaps(left: PeekLayoutItem, right: PeekLayoutItem): boolean {
	return (
		left.column < right.column + right.columnSpan &&
		left.column + left.columnSpan > right.column &&
		left.row < right.row + right.rowSpan &&
		left.row + left.rowSpan > right.row
	);
}
