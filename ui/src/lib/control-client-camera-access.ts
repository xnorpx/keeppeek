import { create } from '@bufbuild/protobuf';
import type { CameraAccessSettings } from './access';
import {
	GetStateSchema,
	PutStateSchema,
	StateStoreCommandSchema,
	type Ok,
	type Request,
	type StateStoreCommand
} from './proto/webrtc_pb';

export const cameraAccessCapability = 'keeppeek.camera-access.v1';
const namespace = 'keeppeek.camera-access';
const encoder = new TextEncoder();
type SendRequest = (command: Request['command']) => Promise<Ok['result']>;

export class CameraAccessControlClient {
	constructor(private readonly sendRequest: SendRequest) {}

	async get(credentialId: string): Promise<CameraAccessSettings> {
		validateIdentity(credentialId);
		return this.entry(
			credentialId,
			create(StateStoreCommandSchema, {
				action: { case: 'get', value: create(GetStateSchema, { namespace, key: credentialId }) }
			})
		);
	}

	async save(settings: CameraAccessSettings): Promise<CameraAccessSettings> {
		validateIdentity(settings.credentialId);
		parsePolicy(settings.allCameras, settings.groupIds, settings.cameraIds);
		if (settings.revision <= 0n) throw new Error('Camera access revision is required.');
		return this.entry(
			settings.credentialId,
			create(StateStoreCommandSchema, {
				action: {
					case: 'put',
					value: create(PutStateSchema, {
						namespace,
						key: settings.credentialId,
						schema: cameraAccessCapability,
						expectedRevision: settings.revision,
						value: {
							all_cameras: settings.allCameras,
							group_ids: [...settings.groupIds],
							camera_ids: [...settings.cameraIds]
						}
					})
				}
			})
		);
	}

	private async entry(
		credentialId: string,
		command: StateStoreCommand
	): Promise<CameraAccessSettings> {
		const result = await this.sendRequest({ case: 'stateStoreCommand', value: command });
		if (result.case !== 'stateStoreResult' || result.value.result.case !== 'entry') {
			throw new Error('Server returned an unexpected camera access response.');
		}
		const entry = result.value.result.value;
		if (
			entry.namespace !== namespace ||
			entry.key !== credentialId ||
			entry.ownerId !== credentialId ||
			entry.schema !== cameraAccessCapability ||
			entry.revision <= 0n
		) {
			throw new Error('Server returned unexpected camera access metadata.');
		}
		const value = entry.value;
		if (
			!value ||
			Object.keys(value).some(
				(key) => !['all_cameras', 'group_ids', 'camera_ids', 'available_group_ids'].includes(key)
			)
		)
			throw new Error('Camera access policy is invalid.');
		const policy = parsePolicy(
			value.all_cameras,
			value.group_ids === undefined ? [] : value.group_ids,
			value.camera_ids
		);
		return {
			credentialId,
			...policy,
			availableGroupIds: parseIds(
				value.available_group_ids === undefined ? [] : value.available_group_ids
			),
			revision: entry.revision
		};
	}
}

function validateIdentity(value: string): void {
	if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(value)) {
		throw new Error('Credential identity is invalid.');
	}
}

function parsePolicy(allCameras: unknown, groupValues: unknown, cameraValues: unknown) {
	const groupIds = parseIds(groupValues);
	const cameraIds = parseIds(cameraValues);
	if (
		typeof allCameras !== 'boolean' ||
		(allCameras && (groupIds.length !== 0 || cameraIds.length !== 0))
	) {
		throw new Error('Camera access policy is invalid.');
	}
	return { allCameras, groupIds, cameraIds };
}

function parseIds(values: unknown): string[] {
	if (!Array.isArray(values) || values.length > 128 || new Set(values).size !== values.length) {
		throw new Error('User access IDs are invalid.');
	}
	return values.map((id) => {
		if (
			typeof id !== 'string' ||
			!id.trim() ||
			encoder.encode(id).length > 256 ||
			Array.from(id).some((character) => {
				const code = character.charCodeAt(0);
				return code <= 31 || (code >= 127 && code <= 159);
			})
		)
			throw new Error('User access IDs are invalid.');
		return id;
	});
}
