import { create, type JsonObject } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import { PeekLayoutControlClient } from './control-client-peek-layouts';
import { StateEntrySchema, StateStoreResultSchema, type Ok, type Request } from './proto/webrtc_pb';

const wireRegistry: JsonObject = {
	schema_version: 1,
	active_layout_id: 'default',
	layouts: [
		{
			id: 'default',
			name: 'All cameras',
			scope: 'shared',
			owner_id: 'server',
			activity_focus: true,
			tiles: [
				{
					camera_id: 'front-door',
					column: 1,
					row: 1,
					column_span: 12,
					row_span: 12,
					pinned: true
				}
			]
		},
		{
			id: 'front-entry',
			name: 'Front entry',
			scope: 'shared',
			owner_id: 'server',
			audience: {
				everyone: false,
				credential_ids: ['11111111-1111-4111-8111-111111111111']
			},
			activity_focus: false,
			tiles: []
		}
	]
};

const savedWireRegistry: JsonObject = {
	...wireRegistry,
	layouts: [
		{
			...(wireRegistry.layouts as JsonObject[])[0],
			audience: { everyone: true, credential_ids: [] }
		},
		(wireRegistry.layouts as JsonObject[])[1]
	]
};

function stateResult(revision: bigint): NonNullable<Ok['result']> {
	return {
		case: 'stateStoreResult',
		value: create(StateStoreResultSchema, {
			result: {
				case: 'entry',
				value: create(StateEntrySchema, {
					namespace: 'keeppeek.peek-layouts',
					key: 'registry',
					schema: 'keeppeek.peek-layout-registry.v1',
					value: wireRegistry,
					revision,
					ownerId: 'alice'
				})
			}
		})
	};
}

describe('Peek layout control client', () => {
	it('gets and saves a revisioned principal registry through StateStore', async () => {
		const responses = [stateResult(7n), stateResult(8n)];
		const sent: Request['command'][] = [];
		const client = new PeekLayoutControlClient(async (command) => {
			sent.push(command);
			return responses.shift()!;
		});

		const registry = await client.get();
		expect(registry).toEqual({
			schemaVersion: 1,
			revision: '7',
			activeLayoutId: 'default',
			layouts: [
				{
					id: 'default',
					name: 'All cameras',
					scope: 'shared',
					ownerId: 'server',
					audience: { everyone: true, credentialIds: [] },
					activityFocus: true,
					items: [
						{
							cameraId: 'front-door',
							column: 1,
							row: 1,
							columnSpan: 12,
							rowSpan: 12,
							pinned: true
						}
					]
				},
				{
					id: 'front-entry',
					name: 'Front entry',
					scope: 'shared',
					ownerId: 'server',
					audience: {
						everyone: false,
						credentialIds: ['11111111-1111-4111-8111-111111111111']
					},
					activityFocus: false,
					items: []
				}
			]
		});
		const saved = await client.save({ ...registry, activeLayoutId: 'default' });
		expect(saved.revision).toBe('8');

		expect(sent.map((command) => command.case)).toEqual(['stateStoreCommand', 'stateStoreCommand']);
		const saveCommand = sent[1];
		expect(saveCommand.case).toBe('stateStoreCommand');
		if (saveCommand.case !== 'stateStoreCommand' || saveCommand.value.action.case !== 'put') {
			throw new Error('expected Peek layout StateStore put command');
		}
		expect(saveCommand.value.action.value).toMatchObject({
			namespace: 'keeppeek.peek-layouts',
			key: 'registry',
			schema: 'keeppeek.peek-layout-registry.v1',
			expectedRevision: 7n,
			value: savedWireRegistry
		});
	});
});
