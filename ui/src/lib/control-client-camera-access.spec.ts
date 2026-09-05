import { create, type JsonObject } from '@bufbuild/protobuf';
import { describe, expect, it, vi } from 'vitest';
import { CameraAccessControlClient } from './control-client-camera-access';
import { StateEntrySchema, StateStoreResultSchema, type Ok, type Request } from './proto/webrtc_pb';

const credentialId = '11111111-1111-4111-8111-111111111111';

function response(value: JsonObject, revision = 4n): Ok['result'] {
	return {
		case: 'stateStoreResult',
		value: create(StateStoreResultSchema, {
			result: {
				case: 'entry',
				value: create(StateEntrySchema, {
					namespace: 'keeppeek.camera-access',
					key: credentialId,
					schema: 'keeppeek.camera-access.v1',
					ownerId: credentialId,
					value,
					revision
				})
			}
		})
	};
}

describe('Camera access control client', () => {
	it('reads default-everything access and saves a per-user combination of groups and cameras', async () => {
		const sent: Request['command'][] = [];
		const values = [
			response({
				all_cameras: true,
				group_ids: [],
				camera_ids: [],
				available_group_ids: ['outdoor', 'indoor']
			}),
			response(
				{
					all_cameras: false,
					group_ids: ['outdoor'],
					camera_ids: ['192.0.2.10'],
					available_group_ids: ['outdoor', 'indoor']
				},
				5n
			)
		];
		const client = new CameraAccessControlClient(async (command) => {
			sent.push(command);
			return values.shift()!;
		});
		const settings = await client.get(credentialId);
		expect(settings.allCameras).toBe(true);
		expect(settings.availableGroupIds).toEqual(['outdoor', 'indoor']);
		const saved = await client.save({
			...settings,
			allCameras: false,
			groupIds: ['outdoor'],
			cameraIds: ['192.0.2.10']
		});
		expect(saved.groupIds).toEqual(['outdoor']);
		const command = sent[1];
		if (command.case !== 'stateStoreCommand' || command.value.action.case !== 'put')
			throw new Error('Expected a user-access update');
		expect(command.value.action.value.value).toEqual({
			all_cameras: false,
			group_ids: ['outdoor'],
			camera_ids: ['192.0.2.10']
		});
	});

	it('reads an empty grant set and saves explicit cameras with the credential revision', async () => {
		const sent: Request['command'][] = [];
		const responses = [
			response({ all_cameras: false, camera_ids: [] }),
			response({ all_cameras: false, camera_ids: ['192.0.2.10'] }, 5n)
		];
		const client = new CameraAccessControlClient(async (command) => {
			sent.push(command);
			return responses.shift()!;
		});
		const settings = await client.get(credentialId);
		expect(settings).toEqual({
			credentialId,
			allCameras: false,
			groupIds: [],
			availableGroupIds: [],
			cameraIds: [],
			revision: 4n
		});
		const saved = await client.save({ ...settings, cameraIds: ['192.0.2.10'] });
		expect(saved.revision).toBe(5n);
		expect(saved.cameraIds).toEqual(['192.0.2.10']);
		const command = sent[1];
		if (command.case !== 'stateStoreCommand' || command.value.action.case !== 'put') {
			throw new Error('Expected a StateStore permission update');
		}
		expect(command.value.action.value).toMatchObject({
			namespace: 'keeppeek.camera-access',
			key: credentialId,
			schema: 'keeppeek.camera-access.v1',
			expectedRevision: 4n,
			value: { all_cameras: false, camera_ids: ['192.0.2.10'] }
		});
	});

	it.each<JsonObject>([
		{ all_cameras: 'true', camera_ids: [] },
		{ all_cameras: true, camera_ids: ['192.0.2.10'] },
		{ all_cameras: false, camera_ids: ['192.0.2.10', '192.0.2.10'] },
		{ all_cameras: false, camera_ids: [''] },
		{ all_cameras: false, camera_ids: ['bad\nidentity'] },
		{ all_cameras: true, group_ids: ['outdoor'], camera_ids: [] },
		{ all_cameras: false, group_ids: ['outdoor', 'outdoor'], camera_ids: [] },
		{ all_cameras: false, group_ids: null, camera_ids: [] },
		{ all_cameras: false, group_ids: ['bad\nname'], camera_ids: [] },
		{ all_cameras: false, camera_ids: Array.from({ length: 129 }, (_, index) => `camera-${index}`) }
	])('rejects invalid permission data without treating it as a grant', async (value) => {
		const client = new CameraAccessControlClient(async () => response(value));
		await expect(client.get(credentialId)).rejects.toThrow();
	});

	it('rejects a response for another credential', async () => {
		const client = new CameraAccessControlClient(async () =>
			response({ all_cameras: true, camera_ids: [] })
		);
		await expect(client.get('22222222-2222-4222-8222-222222222222')).rejects.toThrow();
	});

	it('does not send invalid saves or discard a draft after a conflict', async () => {
		const send = vi.fn(async () => {
			throw new Error('camera access changed');
		});
		const client = new CameraAccessControlClient(send);
		const draft = {
			credentialId,
			allCameras: false,
			groupIds: [],
			availableGroupIds: [],
			cameraIds: ['192.0.2.10'],
			revision: 4n
		};
		await expect(client.save({ ...draft, allCameras: true })).rejects.toThrow();
		expect(send).not.toHaveBeenCalled();
		await expect(client.save(draft)).rejects.toThrow('camera access changed');
		expect(draft.cameraIds).toEqual(['192.0.2.10']);
		expect(draft.revision).toBe(4n);
	});
});
