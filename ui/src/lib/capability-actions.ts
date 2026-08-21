import type { ServerCapabilityId } from '$lib/capabilities';

export type CapabilityAction = {
	action: string;
	capability: ServerCapabilityId;
};

export const capabilityActions = {
	createExport: {
		action: 'Create export',
		capability: 'keeppeek.media-export.v1'
	},
	exportMoment: {
		action: 'Export this moment',
		capability: 'keeppeek.media-export.v1'
	},
	inviteSomeone: {
		action: 'Invite someone',
		capability: 'keeppeek.identity.v1'
	},
	newAccessToken: {
		action: 'New access token',
		capability: 'keeppeek.identity.v1'
	},
	remoteSignIn: {
		action: 'Remote sign-in',
		capability: 'keeppeek.identity.v1'
	},
	enableRemoteSignIn: {
		action: 'Turn on remote sign-in',
		capability: 'keeppeek.identity.v1'
	},
	managePeopleAndSessions: {
		action: 'Manage people and sessions',
		capability: 'keeppeek.identity.v1'
	},
	manageAccessTokens: {
		action: 'Manage access tokens',
		capability: 'keeppeek.identity.v1'
	},
	addRule: {
		action: 'Add a rule',
		capability: 'keeppeek.rules.v1'
	},
	sendNotificationTest: {
		action: 'Send a test',
		capability: 'keeppeek.rules.v1'
	},
	manageNotificationChannels: {
		action: 'Manage notification channels',
		capability: 'keeppeek.rules.v1'
	},
	newGroup: {
		action: 'New group',
		capability: 'keeppeek.group-admin.v1'
	},
	manageGroupDefinitions: {
		action: 'Manage group definitions',
		capability: 'keeppeek.group-admin.v1'
	},
	addOffsiteArchive: {
		action: 'Add offsite archive',
		capability: 'keeppeek.offsite-archive.v1'
	},
	bookmarkMoment: {
		action: 'Bookmark',
		capability: 'keeppeek.bookmarks.v1'
	}
} as const satisfies Record<string, CapabilityAction>;
