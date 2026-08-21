import { describe, expect, it } from 'vitest';
import { groupEvidence } from '$lib/groups';

describe('group evidence', () => {
	it('keeps definitions server-owned and administration capability-gated', () => {
		expect(groupEvidence()).toMatchObject({
			definitionOwner: 'server-configuration',
			clientCommands: ['list', 'join', 'leave'],
			adminCapability: 'keeppeek.group-admin.v1',
			directoryRuntime: 'unavailable',
			adminRuntime: 'unavailable',
			groups: null
		});
	});

	it('models every live group as full duplex with no floor control', () => {
		expect(groupEvidence()).toMatchObject({
			fullDuplex: true,
			floorControl: false,
			staticMembersOnly: true
		});
	});

	it('does not derive directory or participant state from capabilities', () => {
		expect(groupEvidence()).toMatchObject({
			inServerCapabilities: false,
			passwordsReturned: false,
			participantState: null
		});
	});
});
