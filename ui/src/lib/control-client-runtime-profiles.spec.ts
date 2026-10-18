import { describe, expect, it, vi } from 'vitest';
import type { StateEntry } from './proto/webrtc_pb';
import { RuntimeProfilesControlClient } from './control-client-runtime-profiles';
import {
	runtimeProfilesNamespace,
	runtimeProfilesRegistrySchema,
	runtimeProfilesRegistryKey,
	runtimeProfilePartSchema,
	type RuntimeProfileManifest
} from './runtime-profiles';
import type { ControlClientStateStore } from './control-client-state-store';

describe('RuntimeProfilesControlClient', () => {
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
