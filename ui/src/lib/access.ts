export type AccessPermission = {
	label: string;
	kind: 'operation' | 'configuration';
	administrator: true;
	user: boolean;
	requiresCapability?: 'keeppeek.media-export.v1';
};

export type AccessRole = 'administrator' | 'user';

export type CameraAccessSettings = {
	credentialId: string;
	allCameras: boolean;
	groupIds: string[];
	cameraIds: string[];
	availableGroupIds: string[];
	revision: bigint;
};

export type AccessSession = {
	id: string;
	principalId: string;
	displayName: string;
	role: AccessRole;
	local: boolean;
	clientClassification: string;
	createdAtMs: number;
	lastActivityAtMs: number;
	absoluteExpiresAtMs: number;
	credentialExpiresAtMs: number | null;
};

export type AccessCredential = {
	id: string;
	name: string;
	description: string | null;
	role: AccessRole;
	createdAtMs: number;
	rotatedAtMs: number | null;
	lastUsedAtMs: number | null;
	expiresAtMs: number | null;
	disabled: boolean;
	revokedAtMs: number | null;
	revision: bigint;
	initialAccessKeyPending: boolean;
};

export type AccessAuditEvent = {
	id: string;
	timestampMs: number;
	principalId: string | null;
	role: AccessRole | null;
	action: string;
	targetId: string | null;
	result: string;
	clientClassification: string;
};

export type AccessConnectionState = {
	status: 'checking' | 'authenticated' | 'sign-in-required' | 'error';
	session: AccessSession | null;
	message: string | null;
	generation: number;
};

export type AccessCredentialInput = {
	name: string;
	description?: string;
	role: AccessRole;
	expiresAtMs?: number;
};

export type IssuedAccessCredential = {
	credential: AccessCredential;
	accessKey: string;
};

export type AccessEvidence = {
	identityCapability: 'keeppeek.identity.v1';
	identityRuntime: 'available';
	enforcementEvidence: 'server-authoritative';
	people: null;
	roles: 'administrator-user';
	sessions: 'available';
	tokens: 'available';
	auditTrail: 'available';
	targetRoles: readonly ['Administrator', 'User'];
	permissions: readonly AccessPermission[];
	baselineModel: {
		local: 'administrator-without-sign-in';
		remote: 'named-bearer-credential';
		roles: 'administrator-user';
		implementedInCurrentServer: true;
	};
};

const permissions = Object.freeze<AccessPermission[]>([
	{ label: 'Watch live video', kind: 'operation', administrator: true, user: true },
	{ label: 'Open stored recordings', kind: 'operation', administrator: true, user: true },
	{ label: 'Operate camera PTZ and presets', kind: 'operation', administrator: true, user: true },
	{
		label: 'Join a group and publish local media',
		kind: 'operation',
		administrator: true,
		user: true
	},
	{
		label: 'Export a clip or still',
		kind: 'operation',
		administrator: true,
		user: false,
		requiresCapability: 'keeppeek.media-export.v1'
	},
	{ label: 'Configure cameras', kind: 'configuration', administrator: true, user: false },
	{
		label: 'Configure storage and services',
		kind: 'configuration',
		administrator: true,
		user: false
	},
	{ label: 'Manage identities and tokens', kind: 'configuration', administrator: true, user: false }
]);

export function accessEvidence(): AccessEvidence {
	return {
		identityCapability: 'keeppeek.identity.v1',
		identityRuntime: 'available',
		enforcementEvidence: 'server-authoritative',
		people: null,
		roles: 'administrator-user',
		sessions: 'available',
		tokens: 'available',
		auditTrail: 'available',
		targetRoles: ['Administrator', 'User'],
		permissions,
		baselineModel: {
			local: 'administrator-without-sign-in',
			remote: 'named-bearer-credential',
			roles: 'administrator-user',
			implementedInCurrentServer: true
		}
	};
}
