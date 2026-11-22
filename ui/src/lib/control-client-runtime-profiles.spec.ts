import { describe, expect, it, vi } from 'vitest';
import { StateStoreErrorCode, type StateEntry } from './proto/webrtc_pb';
import { StateStoreRequestError } from './state-store-error';
import { RuntimeProfilesControlClient } from './control-client-runtime-profiles';
import {
	runtimeProfileCompactRegistrySchema,
	runtimeProfileSelectSchema,
	runtimeProfilesActiveSchema,
	assembleObservedParts,
	observedPartFromEntry,
	runtimeProfileHeaderFromEntry,
	runtimeProfilesNamespace,
	runtimeProfilesRegistrySchema,
	runtimeProfilesRegistryKey,
	runtimeProfilePartSchema,
	type RuntimeProfileManifest
} from './runtime-profiles';
import type { ControlClientStateStore } from './control-client-state-store';

describe('RuntimeProfilesControlClient', () => {
	it('pins the displayed definition even after a newer active header arrives', async () => {
		const put = vi
			.fn()
			.mockResolvedValue(entry('active', runtimeProfilesActiveSchema, { profile_id: 'away' }, 9n));
		const client = new RuntimeProfilesControlClient({ put } as unknown as ControlClientStateStore);
		const selection = {
			revision: '6',
			active_profile_id: null,
			profiles: { away: { id: 'away', name: 'Away', revision: '3', cameras: {} } }
		};
		await client.activate('away', '8', {
			...selection,
			activation_schema: runtimeProfileSelectSchema
		});
		expect(put).toHaveBeenLastCalledWith(runtimeProfilesNamespace, 'active', {
			schema: runtimeProfileSelectSchema,
			value: { profile_id: 'away', definition_revision: '3' },
			expectedRevision: 8n
		});
		await client.activate('away', '8', selection);
		expect(put).toHaveBeenLastCalledWith(runtimeProfilesNamespace, 'active', {
			schema: runtimeProfilesActiveSchema,
			value: { profile_id: 'away' },
			expectedRevision: 6n
		});
		await client.activate(null, '8', selection);
		expect(put).toHaveBeenLastCalledWith(runtimeProfilesNamespace, 'active', {
			schema: runtimeProfilesActiveSchema,
			value: { profile_id: '' },
			expectedRevision: 8n
		});
	});

	it('preserves an untouched definition revision when a different profile is edited', async () => {
		const first = await profile('home', 0, '3');
		const second = await profile('away', 0, '4');
		const manifest = {
			revision: '8',
			active_profile_id: null,
			profiles: Object.fromEntries(
				[first, second].map(({ definition, digest }) => [
					definition.id,
					{
						id: definition.id,
						name: definition.name,
						revision: definition.revision,
						digest,
						target_count: 0,
						part_count: 0
					}
				])
			)
		};
		const put = vi
			.fn()
			.mockResolvedValue(entry('registry', runtimeProfilesRegistrySchema, manifest, 8n));
		const get = vi
			.fn()
			.mockResolvedValue(entry('registry', runtimeProfilesRegistrySchema, manifest, 8n));
		const client = new RuntimeProfilesControlClient({
			put,
			get
		} as unknown as ControlClientStateStore);
		await expect(
			client.saveRegistry({
				...manifest,
				profiles: { home: { ...first.definition, revision: '2' } }
			})
		).rejects.toMatchObject({ code: StateStoreErrorCode.CONFLICT, currentRevision: 8n });
		expect(put).not.toHaveBeenCalled();
		await client.saveRegistry({
			...manifest,
			profiles: { home: first.definition, away: { ...second.definition, name: 'Edited' } }
		});
		expect(put).toHaveBeenCalledExactlyOnceWith(
			runtimeProfilesNamespace,
			'registry',
			expect.objectContaining({
				expectedRevision: 8n,
				value: expect.objectContaining({
					profiles: expect.objectContaining({
						home: expect.objectContaining({ revision: '3', digest: first.digest }),
						away: expect.objectContaining({ revision: '5' })
					})
				})
			})
		);
	});

	it('saves and reads 32 maximum-length IDs and UTF-8 names inside a compact registry', async () => {
		const profiles = Object.fromEntries(
			Array.from({ length: 32 }, (_, i) => {
				const id = 'p'.repeat(126) + String(i).padStart(2, '0');
				return [
					id,
					{ id, name: 'ç•Œ'.repeat(26) + String(i).padStart(2, '0'), revision: '0', cameras: {} }
				];
			})
		);
		let stored = entry(
			'registry',
			runtimeProfilesRegistrySchema,
			{ revision: '0', active_profile_id: null, profiles: {} },
			0n
		);
		const put = vi.fn(
			async (
				_namespace: string,
				key: string,
				input: { schema: string; value: Record<string, unknown> }
			) => {
				const { draft_id: _draft, ...value } = input.value;
				stored = entry(key, input.schema, { ...value, revision: '1' }, 1n);
				return stored;
			}
		);
		const client = new RuntimeProfilesControlClient({
			get: async () => stored,
			put
		} as unknown as ControlClientStateStore);
		const accepted = await client.saveRegistry({
			revision: '0',
			active_profile_id: null,
			profiles
		});
		expect(stored.schema).toBe(runtimeProfileCompactRegistrySchema);
		expect(Object.keys(accepted.profiles)).toEqual(Object.keys(profiles));
		for (const [id, profile] of Object.entries(accepted.profiles)) {
			expect(profile).toEqual({ ...profiles[id], revision: '1' });
		}
	});

	it('reports an asynchronous registry watch read failure to its owner', async () => {
		let notify: (entries: readonly StateEntry[]) => void = () => {};
		const stateStore = {
			watch: async (_namespace: string, options: { listener: typeof notify }) => {
				notify = options.listener;
				return { close: vi.fn() };
			}
		} as unknown as ControlClientStateStore;
		const client = new RuntimeProfilesControlClient(stateStore);
		vi.spyOn(client, 'getRegistry').mockRejectedValue(new Error('Registry read failed.'));
		const onError = vi.fn();
		await client.watchRegistry(vi.fn(), onError);
		notify([entry('registry', runtimeProfilesRegistrySchema, {}, 3n)]);
		await vi.waitFor(() =>
			expect(onError).toHaveBeenCalledExactlyOnceWith(
				expect.objectContaining({ message: 'Registry read failed.' })
			)
		);
	});
	it('reads stable camera targets without imposing the per-profile 127-target limit', async () => {
		const cameras = Object.fromEntries(
			Array.from({ length: 200 }, (_, index) => [
				`00000000-0000-4000-8000-${String(index + 1).padStart(12, '0')}`,
				`192.0.2.${index + 1}`
			])
		);
		const stateStore = {
			get: async (namespace: string, key: string) => {
				expect(namespace).toBe(runtimeProfilesNamespace);
				expect(key).toBe('cameras');
				return entry(key, 'keeppeek.runtime-profiles.cameras.v1', { cameras }, 3n);
			}
		} as unknown as ControlClientStateStore;
		expect(await new RuntimeProfilesControlClient(stateStore).getCameras()).toEqual(cameras);
	});

	it.each([
		{ cameras: { '192.0.2.1': '192.0.2.1' } },
		{ cameras: { '00000000-0000-4000-8000-000000000001': 'camera.example' } },
		{ cameras: { '00000000-0000-4000-8000-000000000001': '192.0.2.999' } },
		{
			cameras: {
				'00000000-0000-4000-8000-000000000001': '192.0.2.1',
				'00000000-0000-4000-8000-000000000002': '192.0.2.1'
			}
		}
	])('rejects ambiguous or invalid camera bindings %j', async (value) => {
		const stateStore = {
			get: async () => entry('cameras', 'keeppeek.runtime-profiles.cameras.v1', value, 1n)
		} as unknown as ControlClientStateStore;
		await expect(new RuntimeProfilesControlClient(stateStore).getCameras()).rejects.toThrow();
	});

	it('assembles one verified registry generation with at most four part reads at once', async () => {
		const definitions = [await profile('away', 35), await profile('night', 35)];
		const manifest = {
			revision: '4',
			active_profile_id: 'away',
			profiles: Object.fromEntries(
				definitions.map(({ definition, digest }) => [
					definition.id,
					{
						id: definition.id,
						name: definition.name,
						revision: definition.revision,
						target_count: Object.keys(definition.cameras).length,
						part_count: Math.ceil(Object.keys(definition.cameras).length / 16),
						digest
					}
				])
			)
		};
		let activeGets = 0;
		let maxActiveGets = 0;
		const stateStore = {
			get: async (_namespace: string, key: string) => {
				if (key === 'registry')
					return entry('registry', runtimeProfilesRegistrySchema, manifest, 4n);
				activeGets += 1;
				maxActiveGets = Math.max(maxActiveGets, activeGets);
				await new Promise((resolve) => setTimeout(resolve, 0));
				activeGets -= 1;
				const match = /^profiles\/([^/]+)\/(\d+)\/(\d+)$/.exec(key);
				if (match === null) throw new Error(`Unexpected key ${key}`);
				const [, profileId, generation, rawIndex] = match;
				const index = Number(rawIndex);
				const found = definitions.find(({ definition }) => definition.id === profileId);
				if (found === undefined) throw new Error(`Unknown profile ${profileId}`);
				const cameraEntries = Object.entries(found.definition.cameras).slice(
					index * 16,
					(index + 1) * 16
				);
				return entry(
					key,
					'keeppeek.runtime-profiles.part.v1',
					{
						profile_id: profileId,
						generation,
						part_index: index,
						part_count: 3,
						digest: found.digest,
						cameras: Object.fromEntries(cameraEntries)
					},
					2n
				);
			}
		} as unknown as ControlClientStateStore;

		const registry = await new RuntimeProfilesControlClient(stateStore).getRegistry();

		expect(Object.keys(registry.profiles.away.cameras)).toHaveLength(35);
		expect(registry.profiles.night.revision).toBe('2');
		expect(maxActiveGets).toBeLessThanOrEqual(4);
	});

	it('saves large registries as bounded profile parts before the revisioned commit', async () => {
		const initialManifest: RuntimeProfileManifest = {
			revision: '0',
			active_profile_id: null,
			profiles: {}
		};
		let manifest: RuntimeProfileManifest = initialManifest;
		let manifestEntryRevision = 0n;
		const writes: Array<{ key: string; schema: string; value: Record<string, unknown> }> = [];
		const stateStore = {
			get: async (_namespace: string, key: string) => {
				if (key === runtimeProfilesRegistryKey) {
					return entry(key, runtimeProfilesRegistrySchema, manifest, manifestEntryRevision);
				}
				const match = /^profiles\/([^/]+)\/(\d+)\/(\d+)$/.exec(key);
				if (match === null) throw new Error(`Unexpected key ${key}`);
				const [, profileId, generation, rawIndex] = match;
				const index = Number(rawIndex);
				const staged = writes.find(({ key: stagedKey }) =>
					stagedKey.endsWith(`/${profileId}/${index}`)
				);
				if (staged === undefined) throw new Error(`Missing staged part ${key}`);
				const profile = manifest.profiles[profileId];
				if (profile === undefined) throw new Error(`Unknown profile ${profileId}`);
				return entry(
					key,
					runtimeProfilePartSchema,
					{
						profile_id: profileId,
						generation,
						part_index: index,
						part_count: profile.part_count,
						digest: profile.digest,
						cameras: staged.value.cameras
					},
					1n
				);
			},
			put: async (_namespace: string, key: string, input: { schema: string; value: unknown }) => {
				const value = input.value as Record<string, unknown>;
				writes.push({ key, schema: input.schema, value });
				if (key === runtimeProfilesRegistryKey) {
					const { draft_id: _draftId, ...committed } = value;
					manifest = {
						...(committed as unknown as RuntimeProfileManifest),
						revision: '1'
					};
					manifestEntryRevision = 1n;
				}
				return entry(key, input.schema, value, 1n);
			}
		} as unknown as ControlClientStateStore;
		const { definition } = await profile('away', 35, '1');

		const saved = await new RuntimeProfilesControlClient(stateStore).saveRegistry({
			revision: '0',
			active_profile_id: null,
			profiles: { away: definition }
		});

		const stagedWrites = writes.filter(({ key }) => key.startsWith('drafts/'));
		const commit = writes.find(({ key }) => key === runtimeProfilesRegistryKey);
		expect(stagedWrites).toHaveLength(3);
		expect(stagedWrites.map(({ value }) => Object.keys(value.cameras as object).length)).toEqual([
			16, 16, 3
		]);
		expect(commit?.value).toMatchObject({ revision: '0', active_profile_id: null });
		expect(saved.revision).toBe('1');
		expect(Object.keys(saved.profiles.away.cameras)).toHaveLength(35);
	});
});

function observedPart(index: number, generation = 'a'.repeat(64), revision = 3n): StateEntry {
	return entry(
		`observed/${index}`,
		'keeppeek.runtime-profiles.observed.v1',
		{
			generation,
			part_index: index,
			part_count: 8,
			definition_revision: '2',
			revision: revision.toString(),
			active_profile_id: 'away',
			cameras: {}
		},
		revision
	);
}

describe('runtime profile observations', () => {
	it('returns Pending when all three bounded reads cross observation generations', async () => {
		const get = vi.fn(async (_namespace: string, key: string) => {
			const index = Number(key.split('/')[1]);
			return observedPart(index, (index === 0 ? 'a' : 'b').repeat(64));
		});
		const client = new RuntimeProfilesControlClient({ get } as unknown as ControlClientStateStore);
		expect(await client.getStatus()).toBeNull();
		expect(get).toHaveBeenCalledTimes(24);
	});
	it('reads the fresh header before picking a status format on reconnect', async () => {
		const active = entry(
			'active',
			'keeppeek.runtime-profiles.active.v1',
			{ profile_id: 'away', observed_parts: 8, degraded_cameras: 300 },
			3n
		);
		const watch = vi.fn(
			async (
				_ns: string,
				options: { keyPrefix: string; listener: (values: StateEntry[]) => void }
			) => {
				if (options.keyPrefix !== 'active') throw new Error('unexpected legacy status watch');
				options.listener([active]);
				return { close: async () => {} };
			}
		);
		const get = vi.fn(async () => active);
		const client = new RuntimeProfilesControlClient({
			get,
			watch
		} as unknown as ControlClientStateStore);
		const retained = await client.watchActive(vi.fn());
		client.handleConnectionClosed();
		const render = vi.fn();
		const shell = await client.watchShellProfile(render);
		expect(get).toHaveBeenCalledExactlyOnceWith(runtimeProfilesNamespace, 'active');
		expect(watch).toHaveBeenCalledTimes(1);
		expect(render).toHaveBeenCalledExactlyOnceWith('away', 300);
		expect(await client.getActive()).toEqual({ profileId: 'away', revision: '3' });
		await Promise.all([shell.close(), retained.close()]);
	});
	it('validates the whole-fleet degraded count as an unsigned 32-bit value', () => {
		const header = (count: unknown) =>
			entry(
				'active',
				'keeppeek.runtime-profiles.active.v1',
				{
					profile_id: '',
					observed_parts: 8,
					degraded_cameras: count
				},
				1n
			);
		for (const count of [null, 0, 254, 255, 0xffff_ffff]) {
			expect(runtimeProfileHeaderFromEntry(header(count)).degradedCameras).toBe(count);
		}
		for (const count of [-1, 1.5, '255', 0x1_0000_0000]) {
			expect(() => runtimeProfileHeaderFromEntry(header(count))).toThrow('degraded camera count');
		}
	});
	it('validates the observation count on active headers', () => {
		for (const count of [0, 7, 9, '8']) {
			const header = entry(
				'active',
				'keeppeek.runtime-profiles.active.v1',
				{ profile_id: '', observed_parts: count, degraded_cameras: 0 },
				1n
			);
			expect(() => runtimeProfileHeaderFromEntry(header)).toThrow(
				'unsupported observed part count'
			);
		}
	});
	it('assembles compact and expanded observations across the full 254-camera union', () => {
		const compact = (index: number, count: number, flags = 85) => {
			const part = observedPart(index);
			return {
				...part,
				schema: 'keeppeek.runtime-profiles.observed.v2',
				value: {
					...part.value,
					cameras: Object.fromEntries(
						Array.from({ length: count }, (_, i) => [
							String(index * 32 + i),
							['degraded', 'both', flags, 'unavailable']
						])
					)
				}
			};
		};
		const parts = new Map(
			Array.from({ length: 8 }, (_, i) => [
				i,
				observedPartFromEntry(compact(i, i === 7 ? 30 : 32), i)
			])
		);
		const result = assembleObservedParts(parts)!;
		expect(Object.keys(result.cameras)).toHaveLength(254);
		expect(result.cameras['253']).toEqual({
			camera_id: '253',
			state: 'degraded',
			reason: 'unavailable',
			policy: {
				available: true,
				privacy_enforced: false,
				notifications_enabled: true,
				recording_mode: 'both',
				available_source: 'base',
				recording_mode_source: 'profile',
				privacy_source: 'base',
				notifications_source: 'profile'
			}
		});
		parts.set(
			0,
			observedPartFromEntry(
				{
					...observedPart(0),
					value: {
						...observedPart(0).value,
						cameras: Object.fromEntries(Object.entries(result.cameras).slice(0, 32))
					}
				},
				0
			)
		);
		expect(assembleObservedParts(parts)).toEqual(result);
		parts.set(7, observedPartFromEntry(compact(7, 31), 7));
		expect(() => assembleObservedParts(parts)).toThrow('too many');
		expect(() => observedPartFromEntry(compact(0, 33), 0)).toThrow('too many');
		expect(() => observedPartFromEntry(compact(0, 1, 128), 0)).toThrow('flags');
	});
	it('retries a mixed generation and bounds parallel reads', async () => {
		let calls = 0;
		let outstanding = 0;
		let peak = 0;
		const stateStore = {
			get: async (_namespace: string, key: string) => {
				const index = Number(key.split('/')[1]);
				const old = calls++ < 8 && index === 0;
				peak = Math.max(peak, ++outstanding);
				await Promise.resolve();
				outstanding--;
				return observedPart(index, (old ? 'a' : 'b').repeat(64));
			}
		};
		const client = new RuntimeProfilesControlClient(
			stateStore as unknown as ControlClientStateStore
		);
		expect(await client.getStatus()).toEqual({
			revision: '3',
			active_profile_id: 'away',
			cameras: {}
		});
		expect(calls).toBe(16);
		expect(peak).toBeLessThanOrEqual(4);
	});

	it('shares eight watches, emits complete generations and closes after the last listener', async () => {
		const listeners = new Map<number, (entries: StateEntry[]) => void>();
		const close = vi.fn(async () => {});
		const stateStore = {
			watch: async (
				_namespace: string,
				options: { keyPrefix: string; listener: (entries: StateEntry[]) => void }
			) => {
				const index = Number(options.keyPrefix.split('/')[1]);
				listeners.set(index, options.listener);
				options.listener([observedPart(index)]);
				return { close };
			}
		};
		const client = new RuntimeProfilesControlClient(
			stateStore as unknown as ControlClientStateStore
		);
		const first = vi.fn();
		const second = vi.fn();
		const [left, right] = await Promise.all([
			client.watchStatus(first),
			client.watchStatus(second)
		]);
		expect(listeners.size).toBe(8);
		expect(first).toHaveBeenCalledTimes(1);
		expect(second).toHaveBeenCalledTimes(1);
		for (let i = 0; i < 7; i++) listeners.get(i)!([observedPart(i, 'b'.repeat(64), 4n)]);
		expect(first).toHaveBeenCalledTimes(2);
		expect(second).toHaveBeenLastCalledWith(null);
		const late = vi.fn();
		const afterPartial = await client.watchStatus(late);
		expect(late).toHaveBeenCalledExactlyOnceWith(null);
		await afterPartial.close();
		listeners.get(7)!([observedPart(7, 'b'.repeat(64), 4n)]);
		expect(first).toHaveBeenCalledTimes(3);
		expect(second).toHaveBeenLastCalledWith({
			revision: '4',
			active_profile_id: 'away',
			cameras: {}
		});
		await left.close();
		expect(close).not.toHaveBeenCalled();
		await right.close();
		expect(close).toHaveBeenCalledTimes(8);
	});
	it('releases all admitted watches when a competing feature holds the remaining slots', async () => {
		const close = vi.fn(async () => {});
		const watch = vi.fn(async (_namespace: string, options: { keyPrefix: string }) => {
			if (options.keyPrefix === 'observed/7') throw new Error('State store watch limit reached.');
			return { close };
		});
		const client = new RuntimeProfilesControlClient({
			watch
		} as unknown as ControlClientStateStore);
		await expect(client.watchStatus(vi.fn())).rejects.toThrow('watch limit');
		expect(close).toHaveBeenCalledTimes(7);
		await expect(client.watchStatus(vi.fn())).rejects.toThrow('watch limit');
		expect(close).toHaveBeenCalledTimes(14);
	});

	it('reads an older recorder through its legacy status after a typed unknown-key response', async () => {
		const get = async (_namespace: string, key: string) => {
			if (key !== 'status')
				throw new StateStoreRequestError('Unknown key', {
					namespace: runtimeProfilesNamespace,
					key,
					code: StateStoreErrorCode.KEY_INVALID
				});
			return entry(
				key,
				'keeppeek.runtime-profiles.status.v1',
				{ revision: '5', active_profile_id: null, cameras: {} },
				5n
			);
		};
		const client = new RuntimeProfilesControlClient({ get } as unknown as ControlClientStateStore);
		expect((await client.getStatus())?.revision).toBe('5');
	});

	it('rejects an invalid part without trying an older data format', async () => {
		const get = vi.fn(async (_namespace: string, key: string) =>
			observedPart(Number(key.split('/')[1]), 'bad')
		);
		const client = new RuntimeProfilesControlClient({ get } as unknown as ControlClientStateStore);
		await expect(client.getStatus()).rejects.toThrow('observation generation');
		expect(get.mock.calls.some(([, key]) => key === 'status')).toBe(false);
	});
	it('shares the shell header and releases full-fleet watches when the fleet view closes', async () => {
		const closed: string[] = [];
		const watch = vi.fn(
			async (
				_namespace: string,
				options: { keyPrefix: string; listener: (entries: StateEntry[]) => void }
			) => {
				const key = options.keyPrefix;
				options.listener([
					key === 'active'
						? entry(
								key,
								'keeppeek.runtime-profiles.active.v1',
								{ profile_id: 'away', observed_parts: 8, degraded_cameras: 2 },
								3n
							)
						: observedPart(Number(key.split('/')[1]))
				]);
				return {
					close: async () => {
						closed.push(key);
					}
				};
			}
		);
		const client = new RuntimeProfilesControlClient({
			watch
		} as unknown as ControlClientStateStore);
		const render = vi.fn();
		const shell = await client.watchShellProfile(render);
		expect(render).toHaveBeenCalledExactlyOnceWith('away', 2);
		expect(watch).toHaveBeenCalledTimes(1);
		const active = await client.watchActive(vi.fn());
		const fleet = await client.watchStatus(vi.fn());
		expect(watch).toHaveBeenCalledTimes(9);
		await Promise.all([active.close(), fleet.close()]);
		expect(closed.sort()).toEqual(Array.from({ length: 8 }, (_, i) => `observed/${i}`));
		await shell.close();
		expect(closed.at(-1)).toBe('active');
	});
	it('requires fresh parts after reconnect even when the desired revision stays the same', async () => {
		const listeners = new Map<number, (entries: StateEntry[]) => void>();
		const watch = async (
			_namespace: string,
			options: { keyPrefix: string; listener: (entries: StateEntry[]) => void }
		) => {
			const index = Number(options.keyPrefix.split('/')[1]);
			listeners.set(index, options.listener);
			options.listener([observedPart(index)]);
			return { close: async () => {} };
		};
		const client = new RuntimeProfilesControlClient({
			watch
		} as unknown as ControlClientStateStore);
		const render = vi.fn();
		const full = await client.watchStatus(render);
		const update = (index: number) => listeners.get(index)!([observedPart(index, 'b'.repeat(64))]);
		for (let index = 0; index < 7; index++) update(index);
		client.handleConnectionClosed();
		const late = vi.fn();
		const second = await client.watchStatus(late);
		update(7);
		expect(render).toHaveBeenCalledTimes(2);
		expect(late).toHaveBeenCalledExactlyOnceWith(null);
		for (let index = 0; index < 7; index++) update(index);
		expect(late).toHaveBeenCalledTimes(2);
		expect(render).toHaveBeenCalledTimes(3);
		await Promise.all([full.close(), second.close()]);
	});
});

function entry(key: string, schema: string, value: unknown, revision: bigint): StateEntry {
	return {
		namespace: runtimeProfilesNamespace,
		key,
		schema,
		value,
		revision,
		updatedMs: 0n,
		expiresMs: undefined,
		ownerId: 'server'
	} as unknown as StateEntry;
}

async function profile(id: string, count: number, revision = '2') {
	const definition = {
		id,
		name: id,
		revision,
		cameras: Object.fromEntries(
			Array.from({ length: count }, (_, index) => [
				`camera_${String(index).padStart(3, '0')}`,
				{
					availability: index % 2 === 0
				}
			])
		)
	};
	const cameraWire = Object.fromEntries(
		Object.entries(definition.cameras).map(([cameraId, override]) => [
			cameraId,
			{
				availability: override.availability ?? null,
				recording_mode: null,
				privacy_enforced: null,
				notifications_suppressed: null
			}
		])
	);
	const canonical = canonicalJson({
		cameras: Object.fromEntries(Object.entries(cameraWire).sort(([a], [b]) => (a < b ? -1 : 1))),
		id,
		name: id,
		revision
	});
	const bytes = new Uint8Array(
		await crypto.subtle.digest('SHA-256', new TextEncoder().encode(canonical))
	);
	const digest = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
	return { definition, digest };
}

function canonicalJson(value: unknown): string {
	if (value === null || typeof value !== 'object') return JSON.stringify(value);
	if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
	const entries = Object.entries(value).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
	return `{${entries.map(([key, item]) => `${JSON.stringify(key)}:${canonicalJson(item)}`).join(',')}}`;
}
