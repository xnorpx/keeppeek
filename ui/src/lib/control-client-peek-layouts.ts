import { create, type JsonObject } from '@bufbuild/protobuf';
import {
	GetStateSchema,
	PutStateSchema,
	StateStoreCommandSchema,
	type Ok,
	type Request,
	type StateEntry
} from './proto/webrtc_pb';
import type {
	PeekLayout,
	PeekLayoutAudience,
	PeekLayoutItem,
	PeekLayoutRegistry,
	PeekLayoutScope
} from './peek-layout';

const namespace = 'keeppeek.peek-layouts';
const registryKey = 'registry';
const registrySchema = 'keeppeek.peek-layout-registry.v1';
const maxLayouts = 32;
const maxTiles = 64;
const maxViewers = 128;

type SendRequest = (command: Request['command']) => Promise<Ok['result']>;
type UnknownRecord = Record<string, unknown>;

export class PeekLayoutControlClient {
	constructor(private readonly sendRequest: SendRequest) {}

	async get(): Promise<PeekLayoutRegistry> {
		const command = create(StateStoreCommandSchema, {
			action: {
				case: 'get',
				value: create(GetStateSchema, { namespace, key: registryKey })
			}
		});
		return registryFromEntry(await this.entry(command));
	}

	async save(registry: PeekLayoutRegistry): Promise<PeekLayoutRegistry> {
		const command = create(StateStoreCommandSchema, {
			action: {
				case: 'put',
				value: create(PutStateSchema, {
					namespace,
					key: registryKey,
					schema: registrySchema,
					value: registryValue(registry),
					expectedRevision: BigInt(registry.revision)
				})
			}
		});
		return registryFromEntry(await this.entry(command));
	}

	private async entry(
		command: ReturnType<typeof create<typeof StateStoreCommandSchema>>
	): Promise<StateEntry> {
		const result = await this.sendRequest({ case: 'stateStoreCommand', value: command });
		if (result.case !== 'stateStoreResult' || result.value.result.case !== 'entry') {
			throw new Error('Server returned an unexpected Peek layout state response.');
		}
		const entry = result.value.result.value;
		if (
			entry.namespace !== namespace ||
			entry.key !== registryKey ||
			entry.schema !== registrySchema
		) {
			throw new Error('Server returned unexpected Peek layout state metadata.');
		}
		return entry;
	}
}

function registryFromEntry(entry: StateEntry): PeekLayoutRegistry {
	const value = record(entry.value, 'Peek layout registry');
	const schemaVersion = wholeNumber(value.schema_version, 'Peek layout schema version');
	if (schemaVersion !== 1) throw new Error('Server returned an unsupported Peek layout schema.');
	const activeLayoutId = boundedText(value.active_layout_id, 'Active Peek layout ID', 128);
	const wireLayouts = array(value.layouts, 'Peek layouts');
	if (wireLayouts.length === 0 || wireLayouts.length > maxLayouts) {
		throw new Error('Server returned an invalid Peek layout count.');
	}
	const layouts = wireLayouts.map(layoutFromWire);
	const layoutIds = new Set(layouts.map((layout) => layout.id));
	if (layoutIds.size !== layouts.length || !layoutIds.has(activeLayoutId)) {
		throw new Error('Server returned invalid Peek layout identities.');
	}
	return {
		schemaVersion: 1,
		revision: entry.revision.toString(),
		activeLayoutId,
		layouts
	};
}

function layoutFromWire(value: unknown): PeekLayout {
	const wire = record(value, 'Peek layout');
	const id = boundedText(wire.id, 'Peek layout ID', 128);
	const name = boundedText(wire.name, 'Peek layout name', 80);
	const scope = layoutScope(wire.scope);
	const ownerId = boundedText(wire.owner_id, 'Peek layout owner', 128);
	const audience = layoutAudience(wire.audience, scope, ownerId);
	const activityFocus = boolean(wire.activity_focus, 'Peek layout activity focus');
	const wireTiles = array(wire.tiles, 'Peek layout tiles');
	if (wireTiles.length > maxTiles) throw new Error('Server returned too many Peek layout tiles.');
	const items = wireTiles.map(tileFromWire);
	const cameraIds = new Set(items.map((item) => item.cameraId));
	if (cameraIds.size !== items.length) {
		throw new Error('Server returned duplicate cameras in a Peek layout.');
	}
	for (const [index, item] of items.entries()) {
		if (items.slice(0, index).some((other) => overlaps(item, other))) {
			throw new Error('Server returned overlapping Peek layout tiles.');
		}
	}
	return { id, name, scope, ownerId, audience, activityFocus, items };
}

function tileFromWire(value: unknown): PeekLayoutItem {
	const wire = record(value, 'Peek layout tile');
	const item = {
		cameraId: boundedText(wire.camera_id, 'Peek layout camera ID', 256),
		column: wholeNumber(wire.column, 'Peek layout column'),
		row: wholeNumber(wire.row, 'Peek layout row'),
		columnSpan: wholeNumber(wire.column_span, 'Peek layout column span'),
		rowSpan: wholeNumber(wire.row_span, 'Peek layout row span'),
		pinned: boolean(wire.pinned, 'Peek layout pin')
	};
	if (
		item.column < 1 ||
		item.row < 1 ||
		item.columnSpan < 1 ||
		item.rowSpan < 1 ||
		item.column + item.columnSpan > 13 ||
		item.row + item.rowSpan > 13
	) {
		throw new Error('Server returned a Peek layout tile outside the grid.');
	}
	return item;
}

function registryValue(registry: PeekLayoutRegistry): JsonObject {
	return {
		schema_version: registry.schemaVersion,
		active_layout_id: registry.activeLayoutId,
		layouts: registry.layouts.map((layout) => ({
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
		}))
	};
}

function record(value: unknown, label: string): UnknownRecord {
	if (value === null || typeof value !== 'object' || Array.isArray(value)) {
		throw new Error(`Server returned an invalid ${label}.`);
	}
	return value as UnknownRecord;
}

function array(value: unknown, label: string): unknown[] {
	if (!Array.isArray(value)) throw new Error(`Server returned invalid ${label}.`);
	return value;
}

function text(value: unknown, label: string): string {
	if (typeof value !== 'string' || value.trim().length === 0) {
		throw new Error(`Server returned an invalid ${label}.`);
	}
	return value;
}

function boundedText(value: unknown, label: string, maximum: number): string {
	const result = text(value, label);
	if ([...result].length > maximum) throw new Error(`Server returned an invalid ${label}.`);
	return result;
}

function boolean(value: unknown, label: string): boolean {
	if (typeof value !== 'boolean') throw new Error(`Server returned an invalid ${label}.`);
	return value;
}

function wholeNumber(value: unknown, label: string): number {
	if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
		throw new Error(`Server returned an invalid ${label}.`);
	}
	return value;
}

function layoutScope(value: unknown): PeekLayoutScope {
	if (value !== 'private' && value !== 'shared') {
		throw new Error('Server returned an invalid Peek layout scope.');
	}
	return value;
}

function layoutAudience(
	value: unknown,
	scope: PeekLayoutScope,
	ownerId: string
): PeekLayoutAudience {
	if (value === undefined) {
		return scope === 'shared'
			? { everyone: true, credentialIds: [] }
			: { everyone: false, credentialIds: [ownerId] };
	}
	const wire = record(value, 'Peek layout audience');
	const everyone = boolean(wire.everyone, 'Peek layout audience');
	const credentialIds = array(wire.credential_ids, 'Peek layout viewer identities').map((id) =>
		boundedText(id, 'Peek layout viewer identity', 128)
	);
	if (
		credentialIds.length > maxViewers ||
		(everyone && credentialIds.length > 0) ||
		new Set(credentialIds).size !== credentialIds.length
	) {
		throw new Error('Server returned an invalid Peek layout audience.');
	}
	return { everyone, credentialIds };
}

function overlaps(left: PeekLayoutItem, right: PeekLayoutItem): boolean {
	return (
		left.column < right.column + right.columnSpan &&
		left.column + left.columnSpan > right.column &&
		left.row < right.row + right.rowSpan &&
		left.row + left.rowSpan > right.row
	);
}
