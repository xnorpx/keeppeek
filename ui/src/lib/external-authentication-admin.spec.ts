import { create } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import {
	ExternalAuthenticationAdmin,
	newExternalSettings,
	validateExternalSettings
} from './external-authentication-admin';
import { ConfigurationPlanSchema, type Request, type Ok } from './proto/webrtc_pb';

describe('external authentication administration', () => {
	it('keeps identity and browser-session pages within the server limit', async () => {
		const commands: Request['command'][] = [];
		const client = new ExternalAuthenticationAdmin(async (command) => {
			commands.push(command);
			throw new Error('captured');
		});
		await expect(client.listIdentities('identity-next')).rejects.toThrow('captured');
		await expect(client.listSessions('session-next')).rejects.toThrow('captured');
		for (const [index, command] of commands.entries()) {
			if (
				command.case !== 'serverCommand' ||
				command.value.action.case !== 'externalAuthentication'
			)
				throw new Error('Wrong command');
			const action = command.value.action.value.action;
			expect(action.case).toBe(index === 0 ? 'listIdentities' : 'listSessions');
			expect(action.value).toMatchObject({
				pageSize: 32,
				pageToken: index === 0 ? 'identity-next' : 'session-next'
			});
		}
		expect(commands).toHaveLength(2);
	});
	it('rejects inline secrets and noncanonical origins before sending a plan', async () => {
		const settings = newExternalSettings();
		settings.allowedOrigins = ['https://recorder.example/'];
		expect(() => validateExternalSettings(settings)).toThrow('exact HTTPS');
		settings.allowedOrigins = ['https://recorder.example'];
		const provider = settings.providers[0];
		provider.providerId = 'company';
		provider.name = 'Company';
		provider.mappings[0].value = 'viewers';
		if (provider.method.case !== 'oidc') throw new Error('Expected OIDC defaults');
		provider.method.value.redirectUri = 'https://recorder.example/auth/callback';
		provider.method.value.issuer = 'https://identity.example';
		provider.method.value.clientId = 'keeppeek';
		provider.method.value.clientSecretReference = 'inline-secret';
		let requests = 0;
		const client = new ExternalAuthenticationAdmin(async () => {
			requests++;
			throw new Error('Unexpected network request');
		});
		await expect(client.plan(settings, 'revision')).rejects.toThrow('reference');
		expect(requests).toBe(0);
		provider.method.value.clientSecretReference = '{secret:OIDC_CLIENT}';
		expect(() => validateExternalSettings(settings)).not.toThrow();
	});
	it('requires a live plan and binds confirmation to the typed apply command', async () => {
		const commands: Request['command'][] = [];
		const client = new ExternalAuthenticationAdmin(async (command) => {
			commands.push(command);
			throw new Error('captured');
		});
		const plan = create(ConfigurationPlanSchema, {
			planId: 'plan',
			configurationRevision: 'revision',
			valid: true,
			expiresAtMs: BigInt(Date.now() + 60_000),
			requiresAdministratorConfirmation: true
		});
		await expect(client.apply(plan)).rejects.toThrow('verification');
		expect(commands).toHaveLength(0);
		await expect(client.apply(plan, 'proof')).rejects.toThrow('captured');
		const command = commands[0];
		if (command.case !== 'configurationCommand' || command.value.action.case !== 'apply')
			throw new Error('Wrong command');
		expect(command.value.action.value.administratorConfirmation).toMatchObject({
			verificationId: 'proof',
			confirm: true
		});
		plan.expiresAtMs = 1n;
		await expect(client.apply(plan, 'proof')).rejects.toThrow('fresh');
		expect(commands).toHaveLength(1);
	});
	it('rejects a mismatched response instead of claiming verification succeeded', async () => {
		const client = new ExternalAuthenticationAdmin(
			async () => ({ case: undefined }) as Ok['result']
		);
		await expect(client.getVerification('proof')).rejects.toThrow('Unexpected');
	});
});
