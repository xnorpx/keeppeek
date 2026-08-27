import { describe, expect, it } from 'vitest';
import { accessEvidence } from '$lib/access';

describe('access evidence', () => {
	it('reports the server-authoritative identity runtime', () => {
		expect(accessEvidence()).toMatchObject({
			identityCapability: 'keeppeek.identity.v1',
			identityRuntime: 'available',
			enforcementEvidence: 'server-authoritative',
			people: null,
			roles: 'administrator-user',
			sessions: 'available',
			tokens: 'available',
			auditTrail: 'available'
		});
	});

	it('models operation separately from configuration in the target matrix', () => {
		const evidence = accessEvidence();

		expect(evidence.targetRoles).toEqual(['Administrator', 'User']);
		expect(evidence.permissions.filter((permission) => permission.user)).toHaveLength(4);
		expect(
			evidence.permissions
				.filter((permission) => permission.kind === 'configuration')
				.every((permission) => !permission.user)
		).toBe(true);
	});

	it('describes the enforced local and remote boundary', () => {
		expect(accessEvidence().baselineModel).toEqual({
			local: 'administrator-without-sign-in',
			remote: 'named-bearer-credential',
			roles: 'administrator-user',
			implementedInCurrentServer: true
		});
	});
});
