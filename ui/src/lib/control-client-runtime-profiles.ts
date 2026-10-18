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
	runtimeProfileDraftPartSchema,
	runtimeProfileManifestFromEntry,
	runtimeProfilePartFromEntry,
	runtimeProfilePartKey,
	runtimeProfileFromEntry,
	maxRuntimeProfilePartCameras,
	runtimeProfileStatusFromEntry,
	runtimeProfileCamerasFromEntry,
	runtimeProfilesCamerasKey,
	type RuntimeProfileCameras,
	type RuntimeProfileManifest,
	type RuntimeProfileStatus,
	type RuntimeProfileRegistry
} from './runtime-profiles';

const profilePartFetchConcurrency = 4;
const registryReadRetries = 2;

export class RuntimeProfilesControlClient {
	constructor(private readonly stateStore: ControlClientStateStore) {}

	async getCameras(): Promise<RuntimeProfileCameras> {
		return runtimeProfileCamerasFromEntry(
			await this.stateStore.get(runtimeProfilesNamespace, runtimeProfilesCamerasKey)
		);
	}

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
		const draftId = crypto.randomUUID();
		const profiles = Object.fromEntries(
			Object.entries(registry.profiles).map(([id, profile]) => [
				id,
				{ ...profile, revision: nextRevision(profile.revision) }
			])
		);
		const candidate = { ...registry, profiles };
		const summaries = await Promise.all(
			Object.values(profiles).map(async (profile) => ({
				profile,
				partCount: Math.ceil(Object.keys(profile.cameras).length / maxRuntimeProfilePartCameras),
				digest: await digestProfile(profile)
			}))
		);
		const partWrites = summaries.flatMap(({ profile, partCount, digest }) => {
			const cameraEntries = Object.entries(profile.cameras).sort(([left], [right]) =>
				left < right ? -1 : left > right ? 1 : 0
			);
			return Array.from({ length: partCount }, (_, partIndex) => {
				const cameras = Object.fromEntries(
					cameraEntries
						.slice(
							partIndex * maxRuntimeProfilePartCameras,
							(partIndex + 1) * maxRuntimeProfilePartCameras
						)
						.map(([cameraId, override]) => [
							cameraId,
							{
								availability: override.availability ?? null,
								recording_mode: override.recording_mode ?? null,
								privacy_enforced: override.privacy_enforced ?? null,
								notifications_suppressed: override.notifications_suppressed ?? null
							}
						])
				);
				return {
					key: `drafts/${draftId}/${profile.id}/${partIndex}`,
					value: {
						draft_id: draftId,
						profile_id: profile.id,
						generation: profile.revision,
						part_index: partIndex,
						part_count: partCount,
						digest,
						cameras
					}
				};
			});
		});
		await mapBounded(partWrites, profilePartFetchConcurrency, async ({ key, value }) =>
			this.stateStore.put(runtimeProfilesNamespace, key, {
				schema: runtimeProfileDraftPartSchema,
				value: value as JsonObject
			})
		);
		const value = {
			draft_id: draftId,
			revision: candidate.revision,
			active_profile_id: candidate.active_profile_id,
			profiles: Object.fromEntries(
				summaries.map(({ profile, partCount, digest }) => [
					profile.id,
					{
						id: profile.id,
						name: profile.name,
						revision: profile.revision,
						target_count: Object.keys(profile.cameras).length,
						part_count: partCount,
						digest
					}
				])
			)
		};
		await this.stateStore.put(runtimeProfilesNamespace, runtimeProfilesRegistryKey, {
			schema: runtimeProfilesRegistrySchema,
			value: value as JsonObject,
			expectedRevision: BigInt(candidate.revision)
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
		listener: (registry: RuntimeProfileRegistry) => void,
		onError: (cause: unknown) => void
	): Promise<StateStoreWatch> {
		let generation = 0;
		return this.stateStore.watch(runtimeProfilesNamespace, {
			keyPrefix: runtimeProfilesRegistryKey,
			maxEntries: 1,
			listener: (entries) => {
				if (!entries.some((entry) => entry.key === runtimeProfilesRegistryKey)) return;
				const requestGeneration = ++generation;
				void this.getRegistry()
					.then((registry) => {
						if (requestGeneration === generation) listener(registry);
					})
					.catch((cause: unknown) => {
						if (requestGeneration === generation) onError(cause);
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

function nextRevision(revision: string): string {
	const value = BigInt(revision);
	if (value < 0n || value >= 9_223_372_036_854_775_807n) {
		throw new Error('Runtime profile revision is exhausted.');
	}
	return (value + 1n).toString();
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
