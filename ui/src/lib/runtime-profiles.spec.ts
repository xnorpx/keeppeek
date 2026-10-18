import { describe, expect, it } from 'vitest';
import type { StateEntry } from './proto/webrtc_pb';
import {
	profileOverrideIsEmpty,
	runtimeProfilesActiveKey,
	runtimeProfilesActiveSchema,
	runtimeProfilesNamespace,
	runtimeProfilesRegistryKey,
	runtimeProfilesRegistrySchema,
	runtimeProfilesStatusKey,
	runtimeProfilesStatusSchema,
	runtimeProfileFromEntry,
	runtimeProfileManifestFromEntry,
	runtimeProfilePartFromEntry,
	runtimeProfileStatusFromEntry,
	runtimeProfileCamerasFromEntry,
	runtimeProfileTargetForSource
} from './runtime-profiles';

function registryEntry(value: Record<string, unknown>, revision = 9n): StateEntry {
	return {
		namespace: runtimeProfilesNamespace,
		key: runtimeProfilesRegistryKey,
		schema: runtimeProfilesRegistrySchema,
		value,
		revision,
		updatedMs: 0n,
		expiresMs: undefined,
		ownerId: 'server'
	} as unknown as StateEntry;
}

function statusEntry(value: Record<string, unknown>): StateEntry {
	return {
		namespace: runtimeProfilesNamespace,
		key: runtimeProfilesStatusKey,
		schema: runtimeProfilesStatusSchema,
		value,
		revision: 9n,
		updatedMs: 0n,
		expiresMs: undefined,
		ownerId: 'server'
	} as unknown as StateEntry;
}

describe('runtime profile state', () => {
	it('resolves IPv4 and IPv6 sources only through explicit stable bindings', () => {
		const first = '00000000-0000-4000-8000-000000000001';
		const second = '00000000-0000-4000-8000-000000000002';
		const entry = {
			...registryEntry({ cameras: { [first]: '192.0.2.10', [second]: '2001:db8::10' } }),
			key: 'cameras',
			schema: 'keeppeek.runtime-profiles.cameras.v1'
		};
		const cameras = runtimeProfileCamerasFromEntry(entry);
		expect(runtimeProfileTargetForSource(cameras, '192.0.2.10')).toBe(first);
		expect(runtimeProfileTargetForSource(cameras, '2001:db8::10')).toBe(second);
		expect(runtimeProfileTargetForSource(cameras, '192.0.2.11')).toBeNull();
		expect(() =>
			runtimeProfileCamerasFromEntry({ ...entry, schema: 'keeppeek.runtime-profiles.cameras.v2' })
		).toThrow(/metadata/);
	});
	it('parses registry summaries with large revisions and bounded parts', () => {
		const registry = runtimeProfileManifestFromEntry(
			registryEntry({
				revision: '9007199254740993',
				active_profile_id: 'away',
				profiles: {
					away: {
						id: 'away',
						name: 'Away',
						revision: '9007199254740992',
						target_count: 17,
						part_count: 2,
						digest: 'a'.repeat(64)
					}
				}
			})
		);

		expect(registry.revision).toBe('9');
		expect(registry.profiles.away.revision).toBe('9007199254740992');
		expect(registry.profiles.away.part_count).toBe(2);
	});

	it('parses bounded profile parts and ignores null optional fields', () => {
		const cameras = runtimeProfilePartFromEntry(
			{
				namespace: runtimeProfilesNamespace,
				key: 'profiles/away/7/0',
				schema: 'keeppeek.runtime-profiles.part.v1',
				value: {
					profile_id: 'away',
					generation: '7',
					part_index: 0,
					part_count: 1,
					digest: 'b'.repeat(64),
					cameras: {
						front_door: { recording_mode: 'main', privacy_enforced: true, availability: null }
					}
				},
				revision: 7n,
				updatedMs: 0n,
				expiresMs: undefined,
				ownerId: 'server'
			} as unknown as StateEntry,
			{ profileId: 'away', generation: '7', index: 0, count: 1, digest: 'b'.repeat(64) }
		);
		expect(cameras.front_door).toEqual({ recording_mode: 'main', privacy_enforced: true });
	});

	it('validates the active profile projection schema and base value', () => {
		const active = runtimeProfileFromEntry({
			namespace: runtimeProfilesNamespace,
			key: runtimeProfilesActiveKey,
			schema: runtimeProfilesActiveSchema,
			value: { profile_id: '' },
			revision: 4n,
			updatedMs: 0n,
			expiresMs: undefined,
			ownerId: 'server'
		} as unknown as StateEntry);

		expect(active).toEqual({ profileId: null, revision: '4' });
	});

	it('identifies when an override should inherit Base', () => {
		expect(profileOverrideIsEmpty({})).toBe(true);
		expect(profileOverrideIsEmpty({ availability: false })).toBe(false);
	});

	it('parses per-camera convergence state and provenance', () => {
		const status = runtimeProfileStatusFromEntry(
			statusEntry({
				revision: '9',
				active_profile_id: 'away',
				cameras: {
					front_door: {
						camera_id: 'front_door',
						state: 'degraded',
						policy: {
							available: true,
							recording_mode: 'main',
							privacy_enforced: false,
							notifications_enabled: true,
							available_source: 'base',
							recording_mode_source: 'profile',
							privacy_source: 'base',
							notifications_source: 'base'
						},
						reason: 'worker failed'
					}
				}
			})
		);

		expect(status.cameras.front_door.state).toBe('degraded');
		expect(status.cameras.front_door.policy.recording_mode_source).toBe('profile');
		expect(status.cameras.front_door.reason).toBe('worker failed');
	});
});
