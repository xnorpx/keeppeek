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
	runtimeProfileRegistryFromEntry,
	runtimeProfileRegistryToWire,
	runtimeProfileStatusFromEntry
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
	it('round trips large revisions as decimal strings and preserves sparse overrides', () => {
		const registry = runtimeProfileRegistryFromEntry(
			registryEntry({
				revision: '9007199254740993',
				active_profile_id: 'away',
				profiles: {
					away: {
						id: 'away',
						name: 'Away',
						revision: '9007199254740992',
						cameras: {
							front_door: { recording_mode: 'main', privacy_enforced: true }
						}
					}
				}
			})
		);

		expect(registry.revision).toBe('9');
		expect(registry.profiles.away.revision).toBe('9007199254740992');
		expect(registry.profiles.away.cameras.front_door).toEqual({
			recording_mode: 'main',
			privacy_enforced: true
		});
		expect(runtimeProfileRegistryToWire(registry)).toMatchObject({
			revision: '9',
			active_profile_id: 'away'
		});
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
