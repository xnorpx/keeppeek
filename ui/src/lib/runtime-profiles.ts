import type { StateEntry } from './proto/webrtc_pb';

export const runtimeProfilesNamespace = 'service/runtime-profiles/';
export const runtimeProfilesRegistryKey = 'registry';
export const runtimeProfilesActiveKey = 'active';
export const runtimeProfilesRegistrySchema = 'keeppeek.runtime-profiles.registry.v1';
export const runtimeProfilesActiveSchema = 'keeppeek.runtime-profiles.active.v1';
export const runtimeProfilesStatusKey = 'status';
export const runtimeProfilesStatusSchema = 'keeppeek.runtime-profiles.status.v1';
export const maxRuntimeProfiles = 32;
export const maxProfileCameras = 127;

export const profileRecordingModes = [
	'off',
	'sub',
	'main',
	'both',
	'event-boost',
	'event-only'
] as const;

export type ProfileRecordingMode = (typeof profileRecordingModes)[number];

export type CameraProfileOverride = {
	availability?: boolean;
	recording_mode?: ProfileRecordingMode;
	privacy_enforced?: boolean;
	notifications_suppressed?: boolean;
};

export type RuntimeProfile = {
	id: string;
	name: string;
	revision: string;
	cameras: Record<string, CameraProfileOverride>;
};

export type RuntimeProfileRegistry = {
	revision: string;
	active_profile_id: string | null;
	profiles: Record<string, RuntimeProfile>;
};

export type RuntimeProfileStatus = {
	revision: string;
	active_profile_id: string | null;
	cameras: Record<string, CameraRuntimeApplication>;
};

export type CameraRuntimeApplication = {
	camera_id: string;
	state: 'pending' | 'applied' | 'degraded';
	policy: {
		available: boolean;
		recording_mode: ProfileRecordingMode;
		privacy_enforced: boolean;
		notifications_enabled: boolean;
		available_source: 'base' | 'profile';
		recording_mode_source: 'base' | 'profile';
		privacy_source: 'base' | 'profile';
		notifications_source: 'base' | 'profile';
	};
	reason: string | null;
};

type UnknownRecord = Record<string, unknown>;

export function runtimeProfileRegistryFromEntry(entry: StateEntry): RuntimeProfileRegistry {
	if (
		entry.namespace !== runtimeProfilesNamespace ||
		entry.key !== runtimeProfilesRegistryKey ||
		entry.schema !== runtimeProfilesRegistrySchema
	) {
		throw new Error('Server returned unexpected runtime profile registry metadata.');
	}
	const value = record(entry.value, 'runtime profile registry');
	const profilesValue = record(value.profiles, 'runtime profiles');
	const profileEntries = Object.entries(profilesValue);
	if (profileEntries.length > maxRuntimeProfiles) {
		throw new Error('Server returned too many runtime profiles.');
	}
	const profiles: Record<string, RuntimeProfile> = {};
	for (const [key, rawProfile] of profileEntries) {
		const profile = record(rawProfile, 'runtime profile');
		const id = boundedText(profile.id, 'runtime profile ID', 128);
		if (key !== id) throw new Error('Server returned mismatched runtime profile IDs.');
		const camerasValue = record(profile.cameras, 'runtime profile cameras');
		const cameraEntries = Object.entries(camerasValue);
		if (cameraEntries.length > maxProfileCameras) {
			throw new Error('Server returned too many camera overrides.');
		}
		const cameras: Record<string, CameraProfileOverride> = {};
		for (const [cameraId, rawOverride] of cameraEntries) {
			cameras[cameraId] = cameraOverride(rawOverride);
		}
		profiles[id] = {
			id,
			name: boundedText(profile.name, 'runtime profile name', 80),
			revision: revisionText(profile.revision, 'runtime profile revision'),
			cameras
		};
	}
	const activeProfileId = value.active_profile_id;
	if (activeProfileId !== null && typeof activeProfileId !== 'string') {
		throw new Error('Server returned an invalid active runtime profile.');
	}
	if (activeProfileId !== null && profiles[activeProfileId] === undefined) {
		throw new Error('Server returned an unknown active runtime profile.');
	}
	return {
		revision: entry.revision.toString(),
		active_profile_id: activeProfileId,
		profiles
	};
}

export function runtimeProfileRegistryToWire(
	registry: RuntimeProfileRegistry
): Record<string, unknown> {
	return {
		revision: registry.revision,
		active_profile_id: registry.active_profile_id,
		profiles: Object.fromEntries(
			Object.entries(registry.profiles).map(([id, profile]) => [
				id,
				{
					id: profile.id,
					name: profile.name,
					revision: profile.revision,
					cameras: Object.fromEntries(
						Object.entries(profile.cameras).map(([cameraId, override]) => [
							cameraId,
							{ ...override }
						])
					)
				}
			])
		)
	};
}

export function runtimeProfileFromEntry(entry: StateEntry): {
	profileId: string | null;
	revision: string;
} {
	if (
		entry.namespace !== runtimeProfilesNamespace ||
		entry.key !== runtimeProfilesActiveKey ||
		entry.schema !== runtimeProfilesActiveSchema
	) {
		throw new Error('Server returned unexpected active runtime profile metadata.');
	}
	const value = record(entry.value, 'active runtime profile');
	const profileId = value.profile_id;
	if (typeof profileId !== 'string') {
		throw new Error('Server returned an invalid active runtime profile.');
	}
	return {
		profileId: profileId.length === 0 ? null : profileId,
		revision: entry.revision.toString()
	};
}

export function runtimeProfileStatusFromEntry(entry: StateEntry): RuntimeProfileStatus {
	if (
		entry.namespace !== runtimeProfilesNamespace ||
		entry.key !== runtimeProfilesStatusKey ||
		entry.schema !== runtimeProfilesStatusSchema
	) {
		throw new Error('Server returned unexpected runtime profile status metadata.');
	}
	const value = record(entry.value, 'runtime profile status');
	const camerasValue = record(value.cameras, 'runtime profile camera status');
	if (Object.keys(camerasValue).length > maxProfileCameras) {
		throw new Error('Server returned too many runtime profile camera statuses.');
	}
	const cameras: Record<string, CameraRuntimeApplication> = {};
	for (const [cameraId, rawApplication] of Object.entries(camerasValue)) {
		const application = record(rawApplication, 'camera runtime application');
		if (application.camera_id !== cameraId) {
			throw new Error('Server returned mismatched runtime profile camera IDs.');
		}
		if (
			application.state !== 'pending' &&
			application.state !== 'applied' &&
			application.state !== 'degraded'
		) {
			throw new Error('Server returned an invalid camera runtime state.');
		}
		const policy = record(application.policy, 'effective camera policy');
		const source = (field: unknown): 'base' | 'profile' => {
			if (field !== 'base' && field !== 'profile')
				throw new Error('Server returned an invalid policy source.');
			return field;
		};
		if (!profileRecordingModes.includes(policy.recording_mode as ProfileRecordingMode)) {
			throw new Error('Server returned an invalid effective recording mode.');
		}
		cameras[cameraId] = {
			camera_id: boundedText(application.camera_id, 'camera ID', 256),
			state: application.state,
			policy: {
				available: boolean(policy.available),
				recording_mode: policy.recording_mode as ProfileRecordingMode,
				privacy_enforced: boolean(policy.privacy_enforced),
				notifications_enabled: boolean(policy.notifications_enabled),
				available_source: source(policy.available_source),
				recording_mode_source: source(policy.recording_mode_source),
				privacy_source: source(policy.privacy_source),
				notifications_source: source(policy.notifications_source)
			},
			reason:
				application.reason === null
					? null
					: boundedText(application.reason, 'runtime status reason', 256)
		};
	}
	return {
		revision: revisionText(value.revision, 'runtime profile status revision'),
		active_profile_id:
			value.active_profile_id === null
				? null
				: boundedText(value.active_profile_id, 'active runtime profile ID', 128),
		cameras
	};
}

export function profileOverrideIsEmpty(override: CameraProfileOverride): boolean {
	return (
		override.availability === undefined &&
		override.recording_mode === undefined &&
		override.privacy_enforced === undefined &&
		override.notifications_suppressed === undefined
	);
}

function cameraOverride(value: unknown): CameraProfileOverride {
	const wire = record(value, 'camera override');
	const override: CameraProfileOverride = {};
	if (wire.availability !== undefined) override.availability = boolean(wire.availability);
	if (wire.recording_mode !== undefined) {
		if (!profileRecordingModes.includes(wire.recording_mode as ProfileRecordingMode)) {
			throw new Error('Server returned an invalid profile recording mode.');
		}
		override.recording_mode = wire.recording_mode as ProfileRecordingMode;
	}
	if (wire.privacy_enforced !== undefined)
		override.privacy_enforced = boolean(wire.privacy_enforced);
	if (wire.notifications_suppressed !== undefined) {
		override.notifications_suppressed = boolean(wire.notifications_suppressed);
	}
	if (profileOverrideIsEmpty(override))
		throw new Error('Server returned an empty camera override.');
	return override;
}

function record(value: unknown, label: string): UnknownRecord {
	if (value === null || typeof value !== 'object' || Array.isArray(value)) {
		throw new Error(`Server returned an invalid ${label}.`);
	}
	return value as UnknownRecord;
}

function boundedText(value: unknown, label: string, maximum: number): string {
	if (typeof value !== 'string' || value.trim().length === 0 || [...value].length > maximum) {
		throw new Error(`Server returned an invalid ${label}.`);
	}
	return value;
}

function revisionText(value: unknown, label: string): string {
	if (typeof value !== 'string' || !/^(0|[1-9]\d*)$/.test(value)) {
		throw new Error(`Server returned an invalid ${label}.`);
	}
	return value;
}

function boolean(value: unknown): boolean {
	if (typeof value !== 'boolean') throw new Error('Server returned an invalid camera override.');
	return value;
}
