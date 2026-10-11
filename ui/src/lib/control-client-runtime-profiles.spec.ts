import { describe, expect, it } from 'vitest';
import type { StateEntry } from './proto/webrtc_pb';
import { RuntimeProfilesControlClient } from './control-client-runtime-profiles';
import { runtimeProfilesNamespace, runtimeProfilesRegistrySchema } from './runtime-profiles';
import type { ControlClientStateStore } from './control-client-state-store';

describe('RuntimeProfilesControlClient', () => {
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

async function profile(id: string, count: number) {
	const definition = {
		id,
		name: id,
		revision: '2',
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
		revision: '2'
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
