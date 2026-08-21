import { describe, expect, it } from 'vitest';
import {
	isServerCapabilityId,
	serverCapabilityCatalog,
	serverCapabilityIds,
	supportsServerCapability,
	unsupportedCapabilityLabel
} from './capabilities';

describe('server capability contract', () => {
	it('contains the seven exact Board 28 identifiers without duplicates', () => {
		expect(serverCapabilityIds).toEqual([
			'keeppeek.runtime-config.v1',
			'keeppeek.media-export.v1',
			'keeppeek.identity.v1',
			'keeppeek.rules.v1',
			'keeppeek.group-admin.v1',
			'keeppeek.offsite-archive.v1',
			'keeppeek.bookmarks.v1'
		]);
		expect(new Set(serverCapabilityIds).size).toBe(serverCapabilityIds.length);
	});

	it('requires the exact advertised version', () => {
		expect(supportsServerCapability(['keeppeek.media-export.v1'], 'keeppeek.media-export.v1')).toBe(
			true
		);
		expect(supportsServerCapability(['keeppeek.media-export.v2'], 'keeppeek.media-export.v1')).toBe(
			false
		);
		expect(supportsServerCapability([], 'keeppeek.media-export.v1')).toBe(false);
	});

	it('fails closed for unknown capability identifiers', () => {
		expect(isServerCapabilityId('keeppeek.identity.v1')).toBe(true);
		expect(isServerCapabilityId('keeppeek.identity.v2')).toBe(false);
	});

	it('uses the single designed unavailable-command phrase', () => {
		expect(unsupportedCapabilityLabel('keeppeek.rules.v1')).toBe(
			'Server update required · keeppeek.rules.v1'
		);
	});

	it('records ownership, missing behavior, and failure guarantees for every contract', () => {
		for (const capability of serverCapabilityIds) {
			const contract = serverCapabilityCatalog[capability];
			expect(contract.serverOwns.length).toBeGreaterThan(0);
			expect(contract.unlocks.length).toBeGreaterThan(0);
			expect(contract.whenMissing.length).toBeGreaterThan(0);
			expect(contract.failureGuarantee.length).toBeGreaterThan(0);
		}

		expect(serverCapabilityCatalog['keeppeek.runtime-config.v1']).toMatchObject({
			delivery: 'ships',
			unlocks: 'Camera create and update, storage paths, logging, and restart'
		});
	});
});
