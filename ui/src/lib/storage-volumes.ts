import { create } from '@bufbuild/protobuf';
import type { ControlClient } from './control-client';
import {
	StorageVolumeConfigurationSchema,
	StorageVolumeSchema,
	StoragePlacementRuleSchema,
	StorageVolumeRole as Role,
	StorageVolumeState as State,
	StoragePlacementStrategy as Strategy,
	type StorageVolumeConfiguration,
	type StorageVolume,
	type StoragePlacementRule
} from './proto/webrtc_pb';

export type VolumeController = Pick<ControlClient, 'storageVolumes' | 'updateRuntimeConfiguration'>;
export const volumeRoles = [
	{ value: Role.ACTIVE, label: 'Active recordings' },
	{ value: Role.ARCHIVE, label: 'Archive' },
	{ value: Role.EXPORT, label: 'Exports' },
	{ value: Role.THUMBNAIL, label: 'Thumbnails' },
	{ value: Role.METADATA, label: 'Metadata' }
] as const;
export type VolumeDraft = Omit<
	StorageVolume,
	| '$typeName'
	| 'priority'
	| 'capacityBytes'
	| 'minimumFreeBytes'
	| 'warningFreeBytes'
	| 'criticalFreeBytes'
	| 'sources'
	| 'groups'
> & {
	priority: string;
	capacityBytes: string;
	minimumFreeBytes: string;
	warningFreeBytes: string;
	criticalFreeBytes: string;
	sources: string;
	groups: string;
};
export type PlacementDraft = Omit<
	StoragePlacementRule,
	'$typeName' | 'source' | 'group' | 'candidates'
> & {
	source: string;
	group: string;
	candidates: string;
};
export type NamedVolumeDraft = { volumes: VolumeDraft[]; placement: PlacementDraft[] };

export function newVolume(): VolumeDraft {
	return {
		id: '',
		root: '',
		roles: [Role.ARCHIVE],
		state: State.DISABLED,
		priority: '0',
		capacityBytes: '',
		minimumFreeBytes: '0',
		warningFreeBytes: '0',
		criticalFreeBytes: '0',
		sources: '',
		groups: ''
	};
}
export function newPlacement(): PlacementDraft {
	return {
		role: Role.ARCHIVE,
		source: '',
		group: '',
		candidates: '',
		strategy: Strategy.PRIORITY,
		allowFallback: false
	};
}
export function volumeDraft(config?: StorageVolumeConfiguration): NamedVolumeDraft {
	return {
		volumes: (config?.volumes ?? []).map((v) => ({
			id: v.id,
			root: v.root,
			roles: [...v.roles],
			state: v.state,
			priority: v.priority.toString(),
			capacityBytes: v.capacityBytes?.toString() ?? '',
			minimumFreeBytes: v.minimumFreeBytes.toString(),
			warningFreeBytes: v.warningFreeBytes.toString(),
			criticalFreeBytes: v.criticalFreeBytes.toString(),
			sources: v.sources.join('\n'),
			groups: v.groups.join('\n')
		})),
		placement: (config?.placement ?? []).map((r) => ({
			role: r.role,
			source: r.source ?? '',
			group: r.group ?? '',
			candidates: r.candidates.join('\n'),
			strategy: r.strategy,
			allowFallback: r.allowFallback
		}))
	};
}
export function parseBytes(value: string, label: string, minimum = 0n): bigint {
	if (!/^\d+$/.test(value)) throw new Error(`${label} must be a whole number of bytes.`);
	const bytes = BigInt(value);
	if (bytes < minimum || bytes > 9223372036854775807n)
		throw new Error(`${label} must be between ${minimum} and 9223372036854775807 bytes.`);
	return bytes;
}
function lines(value: string): string[] {
	return value
		.split('\n')
		.map((v) => v.trim())
		.filter(Boolean);
}
function encodeVolume(v: VolumeDraft): StorageVolume {
	if (!v.id.trim() || !v.root.trim()) throw new Error('Every volume needs an ID and root.');
	if (!v.roles.length) throw new Error(`Choose at least one role for ${v.id}.`);
	const priority = parseBytes(v.priority, 'Priority');
	if (priority > 65535n) throw new Error('Priority must not exceed 65535.');
	const result = create(StorageVolumeSchema, {
		...v,
		priority: Number(priority),
		capacityBytes: v.capacityBytes === '' ? undefined : parseBytes(v.capacityBytes, 'Capacity', 1n),
		minimumFreeBytes: parseBytes(v.minimumFreeBytes, 'Minimum free space'),
		warningFreeBytes: parseBytes(v.warningFreeBytes, 'Warning free space'),
		criticalFreeBytes: parseBytes(v.criticalFreeBytes, 'Critical free space'),
		sources: lines(v.sources),
		groups: lines(v.groups)
	});
	if (
		result.warningFreeBytes < result.minimumFreeBytes ||
		result.warningFreeBytes < result.criticalFreeBytes
	)
		throw new Error(`Warning free space for ${v.id} must cover minimum and critical free space.`);
	return result;
}
export function encodeDraft(draft: NamedVolumeDraft): StorageVolumeConfiguration {
	if (draft.volumes.length > 32 || draft.placement.length > 256)
		throw new Error('Use at most 32 volumes and 256 placement rules.');
	const volumes = draft.volumes.map(encodeVolume);
	if (new Set(volumes.map((v) => v.id)).size !== volumes.length)
		throw new Error('Volume IDs must be unique.');
	const placement = draft.placement.map((r) => {
		const candidates = lines(r.candidates);
		if (
			!candidates.length ||
			candidates.length > 8 ||
			new Set(candidates).size !== candidates.length
		)
			throw new Error('Each placement rule needs 1 to 8 distinct candidate IDs.');
		if (r.source && r.group)
			throw new Error('A placement rule selects a source or a group, not both.');
		if (candidates.some((id) => !volumes.some((v) => v.id === id && v.roles.includes(r.role))))
			throw new Error('Each candidate must name a volume supporting the rule role.');
		return create(StoragePlacementRuleSchema, {
			...r,
			source: r.source || undefined,
			group: r.group || undefined,
			candidates
		});
	});
	return create(StorageVolumeConfigurationSchema, { volumes, placement });
}

export function draftError(draft: NamedVolumeDraft): string | null {
	try {
		encodeDraft(draft);
		return null;
	} catch (error) {
		return error instanceof Error ? error.message : 'Invalid volume draft.';
	}
}
