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
	runtimeProfileManifestFromEntry,
	runtimeProfilePartFromEntry,
	runtimeProfilePartKey,
	runtimeProfileFromEntry,
	runtimeProfileRegistryToWire,
	runtimeProfileStatusFromEntry,
	type RuntimeProfileManifest,
	type RuntimeProfileStatus,
	type RuntimeProfileRegistry
} from './runtime-profiles';

const profilePartFetchConcurrency = 4;
const registryReadRetries = 2;

export class RuntimeProfilesControlClient {
	constructor(private readonly stateStore: ControlClientStateStore) {}

	async getActive(): Promise<{ profileId: string | null; revision: string }> {
		return runtimeProfileFromEntry(
			await this.stateStore.get(runtimeProfilesNamespace, runtimeProfilesActiveKey)
		);
	}

	async getRegistry(): Promise<RuntimeProfileRegistry> {
		for (let attempt = 0; attempt <= registryReadRetries; attempt += 1) {
			const manifestEntry = await this.stateStore.get(
				runtimeProfilesNamespace,
				runtimeProfilesRegistryKey
			);
			const manifest = runtimeProfileManifestFromEntry(manifestEntry);
			try {
				const registry = await this.readManifest(manifest);
				const current = await this.stateStore.get(
					runtimeProfilesNamespace,
					runtimeProfilesRegistryKey
				);
				if (current.revision === manifestEntry.revision) return registry;
			} catch (error) {
				const current = await this.stateStore.get(
					runtimeProfilesNamespace,
					runtimeProfilesRegistryKey
				);
				if (current.revision === manifestEntry.revision) throw error;
			}
		}
		throw new Error('Runtime profiles changed while they were being read.');
	}

	async getStatus(): Promise<RuntimeProfileStatus> {
		return runtimeProfileStatusFromEntry(
			await this.stateStore.get(runtimeProfilesNamespace, runtimeProfilesStatusKey)
		);
	}

	async saveRegistry(registry: RuntimeProfileRegistry): Promise<RuntimeProfileRegistry> {
		await this.stateStore.put(runtimeProfilesNamespace, runtimeProfilesRegistryKey, {
			schema: runtimeProfilesRegistrySchema,
			value: runtimeProfileRegistryToWire(registry) as JsonObject,
			expectedRevision: BigInt(registry.revision)
		});
		return this.getRegistry();
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

	async watchRegistry(
		listener: (registry: RuntimeProfileRegistry) => void
	): Promise<StateStoreWatch> {
		let generation = 0;
		return this.stateStore.watch(runtimeProfilesNamespace, {
			keyPrefix: runtimeProfilesRegistryKey,
			maxEntries: 1,
			listener: (entries) => {
				if (!entries.some((entry) => entry.key === runtimeProfilesRegistryKey)) return;
				const requestGeneration = ++generation;
				void this.getRegistry().then((registry) => {
					if (requestGeneration === generation) listener(registry);
				});
			}
		});
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

	private async readManifest(manifest: RuntimeProfileManifest): Promise<RuntimeProfileRegistry> {
		const profiles = Object.values(manifest.profiles);
		const partRequests = profiles.flatMap((profile) =>
			Array.from({ length: profile.part_count }, (_, index) => ({ profile, index }))
		);
		const parts = await mapBounded(
			partRequests,
			profilePartFetchConcurrency,
			async ({ profile, index }) => {
				const entry = await this.stateStore.get(
					runtimeProfilesNamespace,
					runtimeProfilePartKey(profile.id, profile.revision, index)
				);
				return {
					profileId: profile.id,
					cameras: runtimeProfilePartFromEntry(entry, {
						profileId: profile.id,
						generation: profile.revision,
						index,
						count: profile.part_count,
						digest: profile.digest
					})
				};
			}
		);
		const definitions = await Promise.all(
			profiles.map(async (profile) => {
				const cameras = Object.assign(
					{},
					...parts.filter((part) => part.profileId === profile.id).map((part) => part.cameras)
				);
				if (Object.keys(cameras).length !== profile.target_count) {
					throw new Error('Server returned an incomplete runtime profile.');
				}
				const definition = {
					id: profile.id,
					name: profile.name,
					revision: profile.revision,
					cameras
				};
				if ((await digestProfile(definition)) !== profile.digest) {
					throw new Error('Server returned a runtime profile with an invalid digest.');
				}
				return [profile.id, definition] as const;
			})
		);
		return {
			revision: manifest.revision,
			active_profile_id: manifest.active_profile_id,
			profiles: Object.fromEntries(definitions)
		};
	}
}

async function mapBounded<T, R>(
	items: readonly T[],
	concurrency: number,
	mapper: (item: T) => Promise<R>
): Promise<R[]> {
	const results = Array.from<R>({ length: items.length });
	let nextIndex = 0;
	const workers = Array.from({ length: Math.min(concurrency, items.length) }, async () => {
		while (nextIndex < items.length) {
			const index = nextIndex++;
			results[index] = await mapper(items[index]);
		}
	});
	await Promise.all(workers);
	return results;
}

async function digestProfile(profile: RuntimeProfileRegistry['profiles'][string]): Promise<string> {
	const cameras = Object.fromEntries(
		Object.entries(profile.cameras).map(([cameraId, override]) => [
			cameraId,
			{
				availability: override.availability ?? null,
				recording_mode: override.recording_mode ?? null,
				privacy_enforced: override.privacy_enforced ?? null,
				notifications_suppressed: override.notifications_suppressed ?? null
			}
		])
	);
	const value = canonicalJson({
		cameras,
		id: profile.id,
		name: profile.name,
		revision: profile.revision
	});
	const bytes = new TextEncoder().encode(value);
	const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', bytes));
	return Array.from(digest, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

function canonicalJson(value: unknown): string {
	if (value === null || typeof value !== 'object') {
		const encoded = JSON.stringify(value);
		if (encoded === undefined) throw new Error('Cannot encode runtime profile digest input.');
		return encoded;
	}
	if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
	const entries = Object.entries(value).sort(([left], [right]) =>
		left < right ? -1 : left > right ? 1 : 0
	);
	return `{${entries.map(([key, item]) => `${JSON.stringify(key)}:${canonicalJson(item)}`).join(',')}}`;
}
