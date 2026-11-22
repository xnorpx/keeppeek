import { fromJson, toBinary, type JsonObject } from '@bufbuild/protobuf';
import { StructSchema } from '@bufbuild/protobuf/wkt';
import { StateStoreErrorCode, type StateEntry } from './proto/webrtc_pb';
import { StateStoreRequestError } from './state-store-error';
import { SharedProfileWatch, type ProfileSubscription } from './runtime-profile-shared-watch';
import type { ControlClientStateStore, StateStoreWatch } from './control-client-state-store';
import {
	assembleObservedParts,
	sameObservationGeneration,
	observedPartCount,
	observedPartFromEntry,
	type ObservedPart,
	runtimeProfileSelectSchema,
	runtimeProfilesActiveKey,
	runtimeProfilesActiveSchema,
	runtimeProfilesNamespace,
	runtimeProfilesRegistryKey,
	runtimeProfileCompactRegistrySchema,
	runtimeProfilesRegistrySchema,
	runtimeProfilesStatusKey,
	runtimeProfileDraftPartSchema,
	runtimeProfileManifestFromEntry,
	runtimeProfilePartFromEntry,
	runtimeProfilePartKey,
	runtimeProfileFromEntry,
	runtimeProfileHeaderFromEntry,
	type ActiveRuntimeProfile,
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
const observedIndices = Array.from({ length: observedPartCount }, (_, index) => index);

type PreparedProfile = {
	profile: RuntimeProfileRegistry['profiles'][string];
	partCount: number;
	digest: string;
};

export class RuntimeProfilesControlClient {
	private readonly activeWatch = new SharedProfileWatch<ActiveRuntimeProfile | null>();
	private readonly statusWatch = new SharedProfileWatch<RuntimeProfileStatus | null>();
	constructor(private readonly stateStore: ControlClientStateStore) {}

	private connectionGeneration = 0;

	handleConnectionClosed(): void {
		this.connectionGeneration++;
		this.activeWatch.pause(null);
		this.statusWatch.pause(null);
	}

	async handleConnectionOpened(): Promise<void> {
		await Promise.all([this.activeWatch.resume(), this.statusWatch.resume()]);
	}

	async getCameras(): Promise<RuntimeProfileCameras> {
		return runtimeProfileCamerasFromEntry(
			await this.stateStore.get(runtimeProfilesNamespace, runtimeProfilesCamerasKey)
		);
	}

	async getActive(): Promise<ReturnType<typeof runtimeProfileFromEntry>> {
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

	private async hasObservedParts(): Promise<boolean> {
		const entry = await this.stateStore.get(runtimeProfilesNamespace, runtimeProfilesActiveKey);
		return runtimeProfileHeaderFromEntry(entry).degradedCameras !== undefined;
	}

	async getStatus(): Promise<RuntimeProfileStatus | null> {
		if (!(await this.hasObservedParts())) {
			return runtimeProfileStatusFromEntry(
				await this.stateStore.get(runtimeProfilesNamespace, runtimeProfilesStatusKey)
			);
		}
		for (let attempt = 0; attempt <= registryReadRetries; attempt++) {
			const parts = await mapBounded(observedIndices, profilePartFetchConcurrency, async (index) =>
				observedPartFromEntry(
					await this.stateStore.get(runtimeProfilesNamespace, `observed/${index}`),
					index
				)
			);
			const assembled = assembleObservedParts(new Map(parts.entries()));
			if (assembled) return assembled;
		}
		return null;
	}

	async saveRegistry(registry: RuntimeProfileRegistry): Promise<RuntimeProfileRegistry> {
		const draftId = crypto.randomUUID();
		const current = runtimeProfileManifestFromEntry(
			await this.stateStore.get(runtimeProfilesNamespace, runtimeProfilesRegistryKey)
		);
		if (
			current.revision !== registry.revision ||
			Object.values(registry.profiles).some((candidate) => {
				const previous = current.profiles[candidate.id];
				return previous && candidate.revision !== '0' && previous.revision !== candidate.revision;
			})
		) {
			throw new StateStoreRequestError('Runtime profiles changed before saving.', {
				namespace: runtimeProfilesNamespace,
				key: runtimeProfilesRegistryKey,
				code: StateStoreErrorCode.CONFLICT,
				currentRevision: BigInt(current.revision)
			});
		}
		const summaries = await this.prepareDefinitions(registry, current);
		await this.stageDraft(draftId, summaries);
		await this.commitRegistry(draftId, registry, summaries);
		return this.getRegistry();
	}

	private prepareDefinitions(
		registry: RuntimeProfileRegistry,
		current: RuntimeProfileManifest
	): Promise<PreparedProfile[]> {
		return mapBounded(
			Object.values(registry.profiles),
			profilePartFetchConcurrency,
			async (candidate) => {
				const previous = current.profiles[candidate.id];
				let profile = { ...candidate, revision: previous?.revision ?? '1' };
				let digest = await digestProfile(profile);
				if (previous && digest !== previous.digest) {
					profile = { ...candidate, revision: nextRevision(previous.revision) };
					digest = await digestProfile(profile);
				}
				return {
					profile,
					partCount: Math.ceil(Object.keys(profile.cameras).length / maxRuntimeProfilePartCameras),
					digest
				};
			}
		);
	}

	private async stageDraft(draftId: string, summaries: PreparedProfile[]): Promise<void> {
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
	}

	private async commitRegistry(
		draftId: string,
		registry: RuntimeProfileRegistry,
		summaries: PreparedProfile[]
	): Promise<void> {
		const value = {
			draft_id: draftId,
			revision: registry.revision,
			active_profile_id: registry.active_profile_id,
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
		const compact =
			toBinary(StructSchema, fromJson(StructSchema, value as JsonObject)).byteLength > 16 * 1024;
		const payload = compact
			? {
					...value,
					profiles: Object.fromEntries(
						Object.entries(value.profiles).map(([id, { id: _duplicateId, ...fields }]) => [
							id,
							fields
						])
					)
				}
			: value;
		const put = (schema: string, document: JsonObject) =>
			this.stateStore.put(runtimeProfilesNamespace, runtimeProfilesRegistryKey, {
				schema,
				value: document,
				expectedRevision: BigInt(registry.revision)
			});
		if (compact) {
			try {
				await put(runtimeProfileCompactRegistrySchema, payload as JsonObject);
			} catch (cause) {
				if (
					!(cause instanceof StateStoreRequestError) ||
					cause.code !== StateStoreErrorCode.SCHEMA_INVALID ||
					cause.namespace !== runtimeProfilesNamespace ||
					cause.key !== runtimeProfilesRegistryKey
				)
					throw cause;
				await put(runtimeProfilesRegistrySchema, value as JsonObject);
			}
		} else {
			await put(runtimeProfilesRegistrySchema, value as JsonObject);
		}
	}

	async activate(
		profileId: string | null,
		expectedRevision: string,
		selection?: RuntimeProfileRegistry
	): Promise<string> {
		return (await this.activateWithHeader(profileId, expectedRevision, selection)).revision;
	}

	async activateWithHeader(
		profileId: string | null,
		expectedRevision: string,
		selection?: RuntimeProfileRegistry
	): Promise<ReturnType<typeof runtimeProfileFromEntry>> {
		let schema = runtimeProfilesActiveSchema;
		const revision = BigInt(expectedRevision);
		const value: JsonObject = { profile_id: profileId ?? '' };
		if (profileId !== null) {
			const displayed =
				selection ??
				runtimeProfileManifestFromEntry(
					await this.stateStore.get(runtimeProfilesNamespace, runtimeProfilesRegistryKey)
				);
			const definition = displayed.profiles[profileId];
			if (!definition) throw new Error('The selected runtime profile is unavailable.');
			const digest = 'digest' in definition ? definition.digest : await digestProfile(definition);
			if (displayed.activation_schema === runtimeProfileSelectSchema) {
				schema = runtimeProfileSelectSchema;
				value.definition_revision = definition.revision;
				value.definition_digest = digest;
			} else if (selection) {
				const latest = runtimeProfileManifestFromEntry(
					await this.stateStore.get(runtimeProfilesNamespace, runtimeProfilesRegistryKey)
				);
				const current = latest.profiles[profileId];
				if (current?.digest !== digest || current.revision !== definition.revision) {
					throw new StateStoreRequestError('The selected profile changed before activation.', {
						namespace: runtimeProfilesNamespace,
						key: runtimeProfilesActiveKey,
						code: StateStoreErrorCode.CONFLICT,
						currentRevision: BigInt(latest.revision)
					});
				}
			}
		}
		const entry = await this.stateStore.put(runtimeProfilesNamespace, runtimeProfilesActiveKey, {
			schema,
			value,
			expectedRevision: revision
		});
		return runtimeProfileFromEntry(entry);
	}

	watchActive(
		listener: (active: ActiveRuntimeProfile | null) => void,
		onError?: (cause: unknown) => void
	): Promise<ProfileSubscription> {
		return this.watchHeader(listener, onError);
	}

	private watchHeader(
		listener: (header: ActiveRuntimeProfile | null) => void,
		onError?: (cause: unknown) => void
	): Promise<ProfileSubscription> {
		return this.activeWatch.subscribe(
			listener,
			async (emit) => {
				let retired = false;
				const generation = this.connectionGeneration;
				const watch = await this.watch(
					runtimeProfilesActiveKey,
					(entry) => emit(runtimeProfileHeaderFromEntry(entry)),
					() => {
						if (retired || generation !== this.connectionGeneration) return;
						retired = true;
						this.activeWatch.pause(null);
						this.statusWatch.pause(null);
						void Promise.all([this.activeWatch.resume(), this.statusWatch.resume()]);
					}
				);
				return {
					close: async () => {
						retired = true;
						await watch.close();
					}
				};
			},
			onError
		);
	}

	async watchShellProfile(
		listener: (profileId: string | null | undefined, degraded: number | null) => void
	): Promise<ProfileSubscription> {
		let header: ActiveRuntimeProfile | undefined;
		let closed = false;
		const legacy = this.legacyShellStatus(() => {
			if (header && !closed) notify(header);
		});
		const notify = (active: ActiveRuntimeProfile) => {
			const status = legacy.value();
			const matches =
				status &&
				status.revision === active.revision &&
				status.active_profile_id === active.profileId &&
				status.definitionRevision === active.definitionRevision;
			const count =
				active.degradedCameras === undefined && matches
					? Object.values(status.cameras).filter((camera) => camera.state === 'degraded').length
					: (active.degradedCameras ?? null);
			listener(active.profileId, count);
		};
		let interrupted = false;
		const accept = (next: ActiveRuntimeProfile | null) => {
			if (closed) return;
			interrupted = next === null;
			if (!next) {
				header = undefined;
				listener(undefined, null);
				void legacy.release();
				return;
			}
			header = next;
			if (next.degradedCameras === undefined) legacy.open();
			else void legacy.release();
			notify(next);
		};
		let active: ProfileSubscription | undefined;
		const close = async () => {
			closed = true;
			await Promise.all([active?.close(), legacy.release()]);
		};
		try {
			active = await this.watchHeader(accept);
			if (!header && !interrupted) {
				const generation = this.connectionGeneration;
				const fresh = runtimeProfileHeaderFromEntry(
					await this.stateStore.get(runtimeProfilesNamespace, runtimeProfilesActiveKey)
				);
				if (!header && !interrupted) {
					if (generation !== this.connectionGeneration)
						throw new Error('Recorder reconnected while reading the active profile.');
					accept(fresh);
				}
			}
			// Legacy camera counts are optional. The selected profile still follows the header.
			await legacy.ready();
			return { close };
		} catch (cause) {
			await close();
			throw cause;
		}
	}

	private legacyShellStatus(notify: () => void) {
		let status: RuntimeProfileStatus | null = null;
		let opening: Promise<ProfileSubscription> | undefined;
		let epoch = 0;
		return {
			value: () => status,
			ready: () => opening?.catch(() => undefined),
			release: async () => {
				epoch++;
				status = null;
				const previous = opening;
				opening = undefined;
				if (previous) await (await previous.catch(() => undefined))?.close();
			},
			open: () => {
				if (opening) return;
				const generation = epoch;
				opening = this.watchStatus((value) => {
					if (generation !== epoch) return;
					status = value;
					notify();
				});
				void opening.catch(() => {
					if (epoch !== generation) return;
					opening = undefined;
					status = null;
					notify();
				});
			}
		};
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

	watchStatus(
		listener: (status: RuntimeProfileStatus | null) => void,
		onError?: (cause: unknown) => void
	): Promise<ProfileSubscription> {
		return this.statusWatch.subscribe(listener, (emit) => this.openStatusWatch(emit), onError);
	}

	private async openStatusWatch(
		emit: (status: RuntimeProfileStatus | null) => void
	): Promise<ProfileSubscription> {
		const generation = this.connectionGeneration;
		let retired = false;
		const interrupt = () => {
			if (retired || generation !== this.connectionGeneration) return;
			retired = true;
			this.statusWatch.pause(null);
			void this.statusWatch.resume();
		};
		const multipart = await this.hasObservedParts();
		if (generation !== this.connectionGeneration) return { close: async () => {} };
		if (!multipart) {
			const watch = await this.watch(
				runtimeProfilesStatusKey,
				(entry) => emit(runtimeProfileStatusFromEntry(entry)),
				interrupt
			);
			return {
				close: async () => {
					retired = true;
					await watch.close();
				}
			};
		}

		emit(null);
		let published: ObservedPart | undefined;
		const parts = new Map<number, ObservedPart>();
		const results = await Promise.allSettled(
			observedIndices.map((index) =>
				this.watch(
					`observed/${index}`,
					(entry) => {
						const part = observedPartFromEntry(entry, index);
						if (published && !sameObservationGeneration(published, part)) {
							published = undefined;
							emit(null);
						}
						parts.set(index, part);
						const status = assembleObservedParts(parts);
						if (status) {
							parts.clear();
							published = part;
							emit(status);
						}
					},
					interrupt
				)
			)
		);
		const close = async () => {
			retired = true;
			await Promise.allSettled(
				results.flatMap((result) => (result.status === 'fulfilled' ? [result.value.close()] : []))
			);
		};
		const failure = results.find((result) => result.status === 'rejected');
		if (failure) {
			await close();
			throw failure.reason;
		}
		return { close };
	}

	private watch(
		key: string,
		listener: (entry: StateEntry) => void,
		onInterrupted?: () => void
	): Promise<StateStoreWatch> {
		let installed = false;
		return this.stateStore.watch(runtimeProfilesNamespace, {
			keyPrefix: key,
			maxEntries: 1,
			onInterrupted,
			listener: (entries) => {
				const entry = entries.find((candidate) => candidate.key === key);
				if (entry !== undefined) {
					listener(entry);
					installed = true;
				} else if (onInterrupted) {
					if (!installed) throw new Error('Missing runtime profile watch snapshot.');
					onInterrupted();
				}
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
			...(manifest.activation_schema ? { activation_schema: manifest.activation_schema } : {}),
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
