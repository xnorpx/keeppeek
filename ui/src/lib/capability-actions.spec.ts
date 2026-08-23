import { describe, expect, it } from 'vitest';
import { capabilityActions } from '$lib/capability-actions';
import { serverCapabilityCatalog, serverCapabilityIds } from '$lib/capabilities';

describe('capability action ownership', () => {
	it('uses every exact Board 28 target capability version', () => {
		const used = new Set(Object.values(capabilityActions).map((action) => action.capability));
		const targetCapabilities = serverCapabilityIds.filter(
			(capability) => serverCapabilityCatalog[capability].delivery !== 'ships'
		);

		expect([...used].toSorted()).toEqual(targetCapabilities.toSorted());
		expect([...used].every((capability) => capability.endsWith('.v1'))).toBe(true);
	});

	it('assigns backend-owned actions to their owning contracts', () => {
		expect(capabilityActions.createExport.capability).toBe('keeppeek.media-export.v1');
		expect(capabilityActions.inviteSomeone.capability).toBe('keeppeek.identity.v1');
		expect(capabilityActions.addRule.capability).toBe('keeppeek.rules.v1');
		expect(capabilityActions.newGroup.capability).toBe('keeppeek.group-admin.v1');
		expect(capabilityActions.addOffsiteArchive.capability).toBe('keeppeek.offsite-archive.v1');
		expect(capabilityActions.bookmarkMoment.capability).toBe('keeppeek.bookmarks.v1');
	});

	it('does not gate shipped runtime configuration writes', () => {
		const capabilities: string[] = Object.values(capabilityActions).map(
			(action) => action.capability
		);

		expect(capabilities).not.toContain('keeppeek.runtime-config.v1');
	});
});
