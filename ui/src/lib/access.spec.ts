import { describe, expect, it } from 'vitest';
import { accessEvidence } from '$lib/access';

describe('access evidence', () => {
	it('keeps identity records and enforcement unavailable', () => {
		expect(accessEvidence()).toMatchObject({
			identityCapability: 'keeppeek.identity.v1',
			identityRuntime: 'unavailable',
			enforcementEvidence: null,
			people: null,
			roles: null,
			sessions: null,
			tokens: null,
			auditTrail: null
		});
	});

	it('models operation separately from configuration in the target matrix', () => {
		const evidence = accessEvidence();

		expect(evidence.targetRoles).toEqual(['Administrator', 'User']);
		expect(evidence.permissions.filter((permission) => permission.user)).toHaveLength(5);
		expect(
			evidence.permissions
				.filter((permission) => permission.kind === 'configuration')
				.every((permission) => !permission.user)
		).toBe(true);
	});

	it('marks the documented shared-key model as unimplemented', () => {
		expect(accessEvidence().documentedPreIdentityModel).toEqual({
			local: 'administrator-without-sign-in',
			remote: 'shared-bearer-key',
			keyScope: 'all-documented-endpoints',
			implementedInCurrentServer: false
		});
	});
});
