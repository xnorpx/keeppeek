import type { JsonObject } from '@bufbuild/protobuf';
import type { StateEntry } from './proto/webrtc_pb';
import type { ControlClientStateStore, StateStoreWatch } from './control-client-state-store';
import {
	runtimeProfilesActiveKey,
	runtimeProfilesActiveSchema,
	runtimeProfilesNamespace,
	runtimeProfilesRegistryKey,
	runtimeProfilesRegistrySchema,
	runtimeProfilesStatusKey,
	runtimeProfileFromEntry,
	runtimeProfileRegistryFromEntry,
	runtimeProfileRegistryToWire,
	runtimeProfileStatusFromEntry,
	type RuntimeProfileStatus,
	type RuntimeProfileRegistry
} from './runtime-profiles';

export class RuntimeProfilesControlClient {
	constructor(private readonly stateStore: ControlClientStateStore) {}

	async getActive(): Promise<{ profileId: string | null; revision: string }> {
		return runtimeProfileFromEntry(
			await this.stateStore.get(runtimeProfilesNamespace, runtimeProfilesActiveKey)
		);
	}

	async getRegistry(): Promise<RuntimeProfileRegistry> {
		return runtimeProfileRegistryFromEntry(
			await this.stateStore.get(runtimeProfilesNamespace, runtimeProfilesRegistryKey)
		);
	}

	async getStatus(): Promise<RuntimeProfileStatus> {
		return runtimeProfileStatusFromEntry(
			await this.stateStore.get(runtimeProfilesNamespace, runtimeProfilesStatusKey)
		);
	}

	async saveRegistry(registry: RuntimeProfileRegistry): Promise<RuntimeProfileRegistry> {
		const entry = await this.stateStore.put(runtimeProfilesNamespace, runtimeProfilesRegistryKey, {
			schema: runtimeProfilesRegistrySchema,
			value: runtimeProfileRegistryToWire(registry) as JsonObject,
			expectedRevision: BigInt(registry.revision)
		});
		return runtimeProfileRegistryFromEntry(entry);
	}

	async activate(profileId: string | null, expectedRevision: string): Promise<string> {
		const entry = await this.stateStore.put(runtimeProfilesNamespace, runtimeProfilesActiveKey, {
			schema: runtimeProfilesActiveSchema,
			value: { profile_id: profileId ?? '' },
			expectedRevision: BigInt(expectedRevision)
		});
		return runtimeProfileFromEntry(entry).revision;
	}

	watchActive(
		listener: (active: ReturnType<typeof runtimeProfileFromEntry>) => void
	): Promise<StateStoreWatch> {
		return this.watch(runtimeProfilesActiveKey, (entry) =>
			listener(runtimeProfileFromEntry(entry))
		);
	}

	watchRegistry(listener: (registry: RuntimeProfileRegistry) => void): Promise<StateStoreWatch> {
		return this.watch(runtimeProfilesRegistryKey, (entry) =>
			listener(runtimeProfileRegistryFromEntry(entry))
		);
	}

	watchStatus(listener: (status: RuntimeProfileStatus) => void): Promise<StateStoreWatch> {
		return this.watch(runtimeProfilesStatusKey, (entry) =>
			listener(runtimeProfileStatusFromEntry(entry))
		);
	}

	private watch(key: string, listener: (entry: StateEntry) => void): Promise<StateStoreWatch> {
		return this.stateStore.watch(runtimeProfilesNamespace, {
			keyPrefix: key,
			maxEntries: 1,
			listener: (entries) => {
				const entry = entries.find((candidate) => candidate.key === key);
				if (entry !== undefined) listener(entry);
			}
		});
	}
}
