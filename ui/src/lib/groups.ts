export type GroupEvidence = {
	definitionOwner: 'server-configuration';
	clientCommands: readonly ['list', 'join', 'leave'];
	adminCapability: 'keeppeek.group-admin.v1';
	directoryRuntime: 'unavailable';
	adminRuntime: 'unavailable';
	groups: null;
	fullDuplex: true;
	floorControl: false;
	staticMembersOnly: true;
	inServerCapabilities: false;
	passwordsReturned: false;
	participantState: null;
};

export function groupEvidence(): GroupEvidence {
	return {
		definitionOwner: 'server-configuration',
		clientCommands: ['list', 'join', 'leave'],
		adminCapability: 'keeppeek.group-admin.v1',
		directoryRuntime: 'unavailable',
		adminRuntime: 'unavailable',
		groups: null,
		fullDuplex: true,
		floorControl: false,
		staticMembersOnly: true,
		inServerCapabilities: false,
		passwordsReturned: false,
		participantState: null
	};
}
