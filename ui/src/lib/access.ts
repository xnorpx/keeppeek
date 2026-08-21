export type AccessPermission = {
	label: string;
	kind: 'operation' | 'configuration';
	administrator: true;
	user: boolean;
	requiresCapability?: 'keeppeek.media-export.v1';
};

export type AccessEvidence = {
	identityCapability: 'keeppeek.identity.v1';
	identityRuntime: 'unavailable';
	enforcementEvidence: null;
	people: null;
	roles: null;
	sessions: null;
	tokens: null;
	auditTrail: null;
	targetRoles: readonly ['Administrator', 'User'];
	permissions: readonly AccessPermission[];
	documentedPreIdentityModel: {
		loopback: 'administrator-without-sign-in';
		remote: 'shared-bearer-key';
		keyScope: 'all-documented-endpoints';
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
		user: true,
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
		identityRuntime: 'unavailable',
		enforcementEvidence: null,
		people: null,
		roles: null,
		sessions: null,
		tokens: null,
		auditTrail: null,
		targetRoles: ['Administrator', 'User'],
		permissions,
		documentedPreIdentityModel: {
			loopback: 'administrator-without-sign-in',
			remote: 'shared-bearer-key',
			keyScope: 'all-documented-endpoints',
			implementedInCurrentServer: true
		}
	};
}
