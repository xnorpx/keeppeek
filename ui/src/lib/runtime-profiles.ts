import type { StateEntry } from './proto/webrtc_pb';

export const runtimeProfilesNamespace = 'service/runtime-profiles/';
export const runtimeProfilesRegistryKey = 'registry';
export const runtimeProfilesActiveKey = 'active';
export const runtimeProfileCompactRegistrySchema = 'keeppeek.runtime-profiles.registry.v3';
export const runtimeProfilesRegistrySchema = 'keeppeek.runtime-profiles.registry.v2';
export const runtimeProfileSelectSchema = 'keeppeek.runtime-profiles.select.v2';
export const runtimeProfilesActiveSchema = 'keeppeek.runtime-profiles.active.v1';
export const runtimeProfilesStatusKey = 'status';
export const runtimeProfilesStatusSchema = 'keeppeek.runtime-profiles.status.v1';
export const runtimeProfilesCamerasKey = 'cameras';
export const runtimeProfilesCamerasSchema = 'keeppeek.runtime-profiles.cameras.v1';
export const runtimeProfilePartSchema = 'keeppeek.runtime-profiles.part.v1';
export const runtimeProfileDraftPartSchema = 'keeppeek.runtime-profiles.draft-part.v1';
export const maxRuntimeProfiles = 32;
export const maxProfileCameras = 127;
export const maxRuntimeProfilePartCameras = 16;
export const observedPartCount = Math.ceil(maxProfileCameras / maxRuntimeProfilePartCameras);
const maxObservedCameras = 2 * maxProfileCameras;
const maxObservedPartCameras = Math.ceil(maxObservedCameras / observedPartCount);

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
	activation_schema?: typeof runtimeProfileSelectSchema;
	revision: string;
	active_profile_id: string | null;
	profiles: Record<string, RuntimeProfile>;
};

export type RuntimeProfileManifest = {
	activation_schema?: typeof runtimeProfileSelectSchema;
	revision: string;
	active_profile_id: string | null;
	profiles: Record<
		string,
		{
			id: string;
			name: string;
			revision: string;
			target_count: number;
			part_count: number;
			digest: string;
		}
	>;
};

export type RuntimeProfileStatus = {
	revision: string;
	active_profile_id: string | null;
	cameras: Record<string, CameraRuntimeApplication>;
};

export type RuntimeProfileCameras = Record<string, string>;

export function runtimeProfileCamerasFromEntry(entry: StateEntry): RuntimeProfileCameras {
	if (
		entry.namespace !== runtimeProfilesNamespace ||
		entry.key !== runtimeProfilesCamerasKey ||
		entry.schema !== runtimeProfilesCamerasSchema
	) {
		throw new Error('Server returned unexpected runtime profile camera metadata.');
	}
	const value = record(entry.value, 'runtime profile cameras');
	const bindings = record(value.cameras, 'runtime profile camera bindings');
	const cameras: RuntimeProfileCameras = {};
	const sources = new Set<string>();
	for (const [id, source] of Object.entries(bindings)) {
		if (
			!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(id) ||
			typeof source !== 'string' ||
			!cameraSourceIsIp(source) ||
			sources.has(source)
		) {
			throw new Error('Server returned invalid or ambiguous runtime profile camera bindings.');
		}
		cameras[id] = source;
		sources.add(source);
	}
	return cameras;
}

export function runtimeProfileTargetForSource(
	cameras: RuntimeProfileCameras,
	source: string
): string | null {
	return Object.entries(cameras).find(([, currentSource]) => currentSource === source)?.[0] ?? null;
}

function cameraSourceIsIp(source: string): boolean {
	if (/^(\d{1,3}\.){3}\d{1,3}$/.test(source)) {
		return source
			.split('.')
			.every((octet) => String(Number(octet)) === octet && Number(octet) <= 255);
	}
	if (!source.includes(':') || source.length > 39) return false;
	try {
		return new URL(`http://[${source}]`).hostname === `[${source}]`;
	} catch {
		return false;
	}
}

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

export function runtimeProfileManifestFromEntry(entry: StateEntry): RuntimeProfileManifest {
	if (
		entry.namespace !== runtimeProfilesNamespace ||
		entry.key !== runtimeProfilesRegistryKey ||
		(entry.schema !== runtimeProfilesRegistrySchema &&
			entry.schema !== runtimeProfileCompactRegistrySchema)
	) {
		throw new Error('Server returned unexpected runtime profile registry metadata.');
	}
	const value = record(entry.value, 'runtime profile registry');
	const profilesValue = record(value.profiles, 'runtime profiles');
	const profileEntries = Object.entries(profilesValue);
	if (profileEntries.length > maxRuntimeProfiles) {
		throw new Error('Server returned too many runtime profiles.');
	}
	const profiles: RuntimeProfileManifest['profiles'] = Object.create(null);
	for (const [key, rawProfile] of profileEntries) {
		const profile = record(rawProfile, 'runtime profile');
		const compact = entry.schema === runtimeProfileCompactRegistrySchema;
		if (compact && Object.hasOwn(profile, 'id'))
			throw new Error('Compact runtime profiles must use the registry key as their ID.');
		const id = boundedText(compact ? key : profile.id, 'runtime profile ID', 128);
		if (key !== id) throw new Error('Server returned mismatched runtime profile IDs.');
		profiles[id] = {
			id,
			name: boundedText(profile.name, 'runtime profile name', 80),
			revision: revisionText(profile.revision, 'runtime profile revision'),
			target_count: boundedInteger(
				profile.target_count,
				'runtime profile target count',
				maxProfileCameras
			),
			part_count: boundedInteger(profile.part_count, 'runtime profile part count', 8),
			digest: digestText(profile.digest, 'runtime profile digest')
		};
		if (
			profiles[id].part_count !==
			Math.ceil(profiles[id].target_count / maxRuntimeProfilePartCameras)
		) {
			throw new Error('Server returned inconsistent runtime profile part count.');
		}
	}
	const activeProfileId = value.active_profile_id;
	if (activeProfileId !== null && typeof activeProfileId !== 'string') {
		throw new Error('Server returned an invalid active runtime profile.');
	}
	if (activeProfileId !== null && profiles[activeProfileId] === undefined) {
		throw new Error('Server returned an unknown active runtime profile.');
	}
	if (
		value.activation_schema !== undefined &&
		value.activation_schema !== runtimeProfileSelectSchema
	) {
		throw new Error('Server returned an unsupported runtime profile activation schema.');
	}
	return {
		...(value.activation_schema === undefined
			? {}
			: { activation_schema: runtimeProfileSelectSchema }),
		revision: entry.revision.toString(),
		active_profile_id: activeProfileId,
		profiles
	};
}

export function runtimeProfilePartKey(
	profileId: string,
	generation: string,
	index: number
): string {
	return `profiles/${profileId}/${generation}/${index}`;
}

export function runtimeProfilePartFromEntry(
	entry: StateEntry,
	expected: { profileId: string; generation: string; index: number; count: number; digest: string }
): Record<string, CameraProfileOverride> {
	const key = runtimeProfilePartKey(expected.profileId, expected.generation, expected.index);
	if (
		entry.namespace !== runtimeProfilesNamespace ||
		entry.key !== key ||
		entry.schema !== runtimeProfilePartSchema
	) {
		throw new Error('Server returned unexpected runtime profile part metadata.');
	}
	const value = record(entry.value, 'runtime profile part');
	if (
		value.profile_id !== expected.profileId ||
		revisionText(value.generation, 'runtime profile part generation') !== expected.generation ||
		boundedInteger(value.part_index, 'runtime profile part index', 7) !== expected.index ||
		boundedInteger(value.part_count, 'runtime profile part count', 8) !== expected.count ||
		digestText(value.digest, 'runtime profile part digest') !== expected.digest
	) {
		throw new Error('Server returned a mismatched runtime profile part.');
	}
	const camerasValue = record(value.cameras, 'runtime profile cameras');
	const cameraEntries = Object.entries(camerasValue);
	if (cameraEntries.length > maxRuntimeProfilePartCameras) {
		throw new Error('Server returned too many camera overrides in a profile part.');
	}
	return Object.fromEntries(
		cameraEntries.map(([cameraId, rawOverride]) => [
			boundedText(cameraId, 'camera ID', 256),
			cameraOverride(rawOverride)
		])
	);
}

export type ActiveRuntimeProfile = {
	profileId: string | null;
	revision: string;
	degradedCameras?: number | null;
};

export function runtimeProfileFromEntry(
	entry: StateEntry
): Pick<ActiveRuntimeProfile, 'profileId' | 'revision'> {
	const { profileId, revision } = runtimeProfileHeaderFromEntry(entry);
	return { profileId, revision };
}

export function runtimeProfileHeaderFromEntry(entry: StateEntry): ActiveRuntimeProfile {
	if (
		entry.namespace !== runtimeProfilesNamespace ||
		entry.key !== runtimeProfilesActiveKey ||
		entry.schema !== runtimeProfilesActiveSchema
	) {
		throw new Error('Server returned unexpected active runtime profile metadata.');
	}
	const value = record(entry.value, 'active runtime profile');
	if (value.observed_parts !== undefined && value.observed_parts !== observedPartCount) {
		throw new Error('Server returned an unsupported observed part count.');
	}
	if (value.revision !== undefined && value.revision !== entry.revision.toString()) {
		throw new Error('Server returned an inconsistent active runtime profile revision.');
	}
	const profileId = value.profile_id;
	if (typeof profileId !== 'string') {
		throw new Error('Server returned an invalid active runtime profile.');
	}
	return {
		profileId: profileId.length === 0 ? null : profileId,
		revision: entry.revision.toString(),
		...(value.observed_parts === undefined
			? {}
			: {
					degradedCameras:
						value.degraded_cameras === null
							? null
							: boundedInteger(value.degraded_cameras, 'degraded camera count', 0xffff_ffff)
				})
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
	return parseStatus(record(entry.value, 'runtime profile status'), maxProfileCameras);
}

export type ObservedPart = {
	generation: string;
	definitionRevision: string | null;
	status: RuntimeProfileStatus;
};

export function sameObservationGeneration(left: ObservedPart, right: ObservedPart): boolean {
	return (
		left.generation === right.generation &&
		left.definitionRevision === right.definitionRevision &&
		left.status.revision === right.status.revision &&
		left.status.active_profile_id === right.status.active_profile_id
	);
}

export function observedPartFromEntry(entry: StateEntry, index: number): ObservedPart {
	const value = record(entry.value, 'runtime profile observations');
	if (
		entry.namespace !== runtimeProfilesNamespace ||
		entry.key !== `observed/${index}` ||
		(entry.schema !== 'keeppeek.runtime-profiles.observed.v1' &&
			entry.schema !== 'keeppeek.runtime-profiles.observed.v2') ||
		value.part_index !== index ||
		value.part_count !== observedPartCount ||
		value.revision !== entry.revision.toString()
	)
		throw new Error('Server returned inconsistent runtime profile observations.');
	return {
		generation: digestText(value.generation, 'observation generation'),
		definitionRevision:
			value.definition_revision === null
				? null
				: revisionText(value.definition_revision, 'active definition revision'),
		status: parseStatus(
			entry.schema === 'keeppeek.runtime-profiles.observed.v2' ? expandCompactPart(value) : value,
			maxObservedPartCameras
		)
	};
}

function expandCompactPart(value: UnknownRecord): UnknownRecord {
	const entries = Object.entries(record(value.cameras, 'compact runtime profile cameras'));
	if (entries.length > maxObservedPartCameras)
		throw new Error('Server returned too many runtime profile camera statuses.');
	const cameras: UnknownRecord = Object.create(null);
	for (const [id, row] of entries) {
		if (!Array.isArray(row) || row.length !== 4)
			throw new Error('Server returned an invalid compact camera observation.');
		const [state, recording_mode, encoded, reason] = row;
		const flags = boundedInteger(encoded, 'camera policy flags', 127);
		const source = (mask: number) => (flags & mask ? 'profile' : 'base');
		cameras[id] = {
			camera_id: id,
			state,
			reason,
			policy: {
				recording_mode,
				available: Boolean(flags & 1),
				privacy_enforced: Boolean(flags & 2),
				notifications_enabled: Boolean(flags & 4),
				available_source: source(8),
				recording_mode_source: source(16),
				privacy_source: source(32),
				notifications_source: source(64)
			}
		};
	}
	return { ...value, cameras };
}

export function assembleObservedParts(
	parts: ReadonlyMap<number, ObservedPart>
): RuntimeProfileStatus | undefined {
	const first = parts.get(0);
	if (!first || parts.size !== observedPartCount) return;
	const cameras: RuntimeProfileStatus['cameras'] = Object.create(null);
	for (let index = 0; index < observedPartCount; index++) {
		const part = parts.get(index);
		if (!part || !sameObservationGeneration(first, part)) return;
		for (const [id, camera] of Object.entries(part.status.cameras)) {
			if (Object.hasOwn(cameras, id))
				throw new Error('Server returned overlapping runtime profile observations.');
			cameras[id] = camera;
		}
	}
	if (Object.keys(cameras).length > maxObservedCameras)
		throw new Error('Server returned too many runtime profile camera statuses.');
	return {
		revision: first.status.revision,
		active_profile_id: first.status.active_profile_id,
		cameras
	};
}

function parseStatus(value: UnknownRecord, maxCameras: number): RuntimeProfileStatus {
	const camerasValue = record(value.cameras, 'runtime profile camera status');
	if (Object.keys(camerasValue).length > maxCameras) {
		throw new Error('Server returned too many runtime profile camera statuses.');
	}
	const cameras: Record<string, CameraRuntimeApplication> = Object.create(null);
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
	if (wire.availability !== undefined && wire.availability !== null)
		override.availability = boolean(wire.availability);
	if (wire.recording_mode !== undefined && wire.recording_mode !== null) {
		if (!profileRecordingModes.includes(wire.recording_mode as ProfileRecordingMode)) {
			throw new Error('Server returned an invalid profile recording mode.');
		}
		override.recording_mode = wire.recording_mode as ProfileRecordingMode;
	}
	if (wire.privacy_enforced !== undefined && wire.privacy_enforced !== null)
		override.privacy_enforced = boolean(wire.privacy_enforced);
	if (wire.notifications_suppressed !== undefined && wire.notifications_suppressed !== null) {
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

function boundedInteger(value: unknown, label: string, maximum: number): number {
	if (typeof value !== 'number' || !Number.isInteger(value) || value < 0 || value > maximum) {
		throw new Error(`Server returned an invalid ${label}.`);
	}
	return value;
}

function digestText(value: unknown, label: string): string {
	if (typeof value !== 'string' || !/^[0-9a-f]{64}$/.test(value)) {
		throw new Error(`Server returned an invalid ${label}.`);
	}
	return value;
}

function boolean(value: unknown): boolean {
	if (typeof value !== 'boolean') throw new Error('Server returned an invalid camera override.');
	return value;
}
