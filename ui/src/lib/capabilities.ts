export type ServerCapabilityContract = {
	delivery: 'ships' | 'required-mvp' | 'target';
	serverOwns: string;
	unlocks: string;
	whenMissing: string;
	failureGuarantee: string;
};

export const serverCapabilityCatalog = {
	'keeppeek.runtime-config.v1': {
		delivery: 'ships',
		serverOwns:
			'Revisioned read, validation, compare-and-set write, restart impact, and atomic apply',
		unlocks: 'Camera create and update, storage paths, logging, and restart',
		whenMissing:
			'Shipped camera, storage, logging, and restart writes stay live; shared defaults remain unavailable',
		failureGuarantee:
			'Draft preserved, zero partial writes, and the current revision returned on conflict'
	},
	'keeppeek.media-export.v1': {
		delivery: 'required-mvp',
		serverOwns:
			'Job creation, keyframe-aligned ranges, progress, cancellation, expiry, and download URLs',
		unlocks: 'Export range and export clip',
		whenMissing: 'Ranges remain selectable and estimated; export commands require a server update',
		failureGuarantee: 'Failed jobs remain retryable and expose no corrupt partial download'
	},
	'stored-media-keyframe-preview.v1': {
		delivery: 'ships',
		serverOwns:
			'One decoder-ready random-access frame per paused SCRUB open or seek, bounded to 4 MiB',
		unlocks: 'Exact-time timeline previews without constructing an MP4 player',
		whenMissing: 'Scrub preview falls back to one keyframe-aligned fMP4 fragment',
		failureGuarantee: 'Generation checks prevent an older preview from replacing the current target'
	},
	'keeppeek.identity.v1': {
		delivery: 'target',
		serverOwns: 'Remote sign-in, sessions, two roles, invitations, and access-token CRUD',
		unlocks: 'Access screens and remote sign-in',
		whenMissing: 'Existing access evidence remains visible; people and roles stay read-only',
		failureGuarantee: 'Failed writes cannot revoke local LAN administration'
	},
	'keeppeek.rules.v1': {
		delivery: 'target',
		serverOwns:
			'Rule and action CRUD, quiet hours, cooldown, test delivery, retries, and delivery history',
		unlocks: 'Notifications and automation actions',
		whenMissing:
			'Channel evidence remains visible; rule creation and tests require a server update',
		failureGuarantee:
			'Test failures never enable a rule; retry count and last response remain visible'
	},
	'keeppeek.group-admin.v1': {
		delivery: 'target',
		serverOwns:
			'Revisioned group definitions, static source validation, password replacement, and recording policy',
		unlocks: 'New group and group editing',
		whenMissing: 'Groups remain listable and joinable; administration is read-only',
		failureGuarantee:
			'A failed edit leaves the previous definition live and never evicts participants'
	},
	'keeppeek.offsite-archive.v1': {
		delivery: 'target',
		serverOwns:
			'Target CRUD, secret replacement, test writes, queueing, retries, reconciliation, and delete policy',
		unlocks: 'Offsite archive locations and copy status',
		whenMissing: 'Existing targets are labelled planned and accept no credentials',
		failureGuarantee:
			'Local recording never blocks on offsite failure; backlog evidence remains visible'
	},
	'keeppeek.bookmarks.v1': {
		delivery: 'target',
		serverOwns: 'Pinning a moment or span through retention, listing it, and removing it',
		unlocks: 'Bookmarks in the event drawer and timeline',
		whenMissing: 'Existing pins stay visible and protected; new pins cannot be made',
		failureGuarantee: 'A failed pin never silently leaves its intended span unprotected'
	}
} as const satisfies Record<string, ServerCapabilityContract>;

export type ServerCapabilityId = keyof typeof serverCapabilityCatalog;

export const serverCapabilityIds = Object.freeze(
	Object.keys(serverCapabilityCatalog) as ServerCapabilityId[]
);

export function isServerCapabilityId(value: string): value is ServerCapabilityId {
	return Object.hasOwn(serverCapabilityCatalog, value);
}

export function supportsServerCapability(
	advertisedCapabilities: Iterable<string>,
	requiredCapability: ServerCapabilityId
): boolean {
	for (const advertisedCapability of advertisedCapabilities) {
		if (advertisedCapability === requiredCapability) return true;
	}
	return false;
}

export function unsupportedCapabilityLabel(capability: ServerCapabilityId): string {
	return `Server update required · ${capability}`;
}
