import { create } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import {
	ConfigurationRestoreVerificationClient,
	validateRestorePreparation
} from './configuration-restore-verification';
import {
	ExternalAuthenticationResultSchema,
	ConfigurationRestoreVerificationSchema,
	type Request
} from './proto/webrtc_pb';

describe('configuration restore preparation', () => {
	it('stops after an in-flight response if the owning session was lost', async () => {
		let changed = false;
		let requests = 0;
		const client = new ConfigurationRestoreVerificationClient(async () => {
			requests++;
			changed = true;
			return {
				case: 'externalAuthenticationResult',
				value: create(ExternalAuthenticationResultSchema, {
					result: {
						case: 'restoreVerification',
						value: create(ConfigurationRestoreVerificationSchema, {
							preparationId: 'id',
							archiveBytes: 3n,
							expiresAtMs: BigInt(Date.now() + 10000)
						})
					}
				})
			};
		});
		await expect(
			client.prepare(new File(['zip'], 'config.zip'), () => {
				if (changed) throw new Error('session changed');
			})
		).rejects.toThrow('session changed');
		expect(requests).toBe(1);
	});
	it('rejects unrelated typed results', async () => {
		const client = new ConfigurationRestoreVerificationClient(async () => ({ case: undefined }));
		await expect(client.get('preparation')).rejects.toThrow('Unexpected restore verification');
	});
	it('rejects oversized chunks without a request and encodes explicit proof confirmation', async () => {
		const commands: Request['command'][] = [];
		const client = new ConfigurationRestoreVerificationClient(async (command) => {
			commands.push(command);
			throw new Error('captured');
		});
		expect(() => client.append('id', 0n, new Uint8Array(32769))).toThrow('chunk');
		expect(() => client.append('id', 0n, new Uint8Array())).toThrow('chunk');
		expect(commands).toHaveLength(0);
		await expect(client.confirm('preparation', 'proof')).rejects.toThrow('captured');
		const command = commands[0];
		if (command.case !== 'serverCommand' || command.value.action.case !== 'externalAuthentication')
			throw new Error('Wrong envelope');
		const action = command.value.action.value.action;
		if (action.case !== 'confirmRestoreVerification') throw new Error('Wrong action');
		expect(action.value).toMatchObject({
			preparationId: 'preparation',
			confirm: true,
			administratorConfirmation: { verificationId: 'proof', confirm: true }
		});
	});
	it('rejects stale, mismatched, and incomplete acknowledgements', () => {
		const result = create(ConfigurationRestoreVerificationSchema, {
			preparationId: 'id',
			archiveBytes: 3n,
			receivedBytes: 3n,
			expiresAtMs: BigInt(Date.now() + 10000)
		});
		expect(() => validateRestorePreparation(result, 'other', 3, 3)).toThrow('matches');
		expect(() => validateRestorePreparation(result, 'id', 3, 2)).toThrow('matches');
		result.expiresAtMs = 1n;
		expect(() => validateRestorePreparation(result, 'id', 3, 3)).toThrow('expired');
	});
	it('hashes the exact archive and appends bounded chunks at exact offsets', async () => {
		const commands: Request['command'][] = [];
		let received = 0n;
		const client = new ConfigurationRestoreVerificationClient(async (command) => {
			commands.push(command);
			if (
				command.case !== 'serverCommand' ||
				command.value.action.case !== 'externalAuthentication'
			)
				throw new Error('Wrong envelope');
			const action = command.value.action.value.action;
			if (action.case === 'appendRestoreVerification') received += BigInt(action.value.data.length);
			return {
				case: 'externalAuthenticationResult',
				value: create(ExternalAuthenticationResultSchema, {
					result: {
						case: 'restoreVerification',
						value: create(ConfigurationRestoreVerificationSchema, {
							preparationId: 'preparation',
							archiveBytes: 32769n,
							receivedBytes: received,
							expiresAtMs: BigInt(Date.now() + 60_000),
							ready: received === 32769n
						})
					}
				})
			};
		});
		const file = new File([new Uint8Array(32769).fill(7)], 'configuration.zip');
		const result = await client.prepare(file, () => {});
		expect(result.ready).toBe(true);
		const actions = commands.map((command) =>
			command.case === 'serverCommand' && command.value.action.case === 'externalAuthentication'
				? command.value.action.value.action
				: null
		);
		expect(actions.map((action) => action?.case)).toEqual([
			'beginRestoreVerification',
			'appendRestoreVerification',
			'appendRestoreVerification',
			'getRestoreVerification'
		]);
		if (actions[0]?.case !== 'beginRestoreVerification') throw new Error('Missing begin');
		const digest = await crypto.subtle.digest('SHA-256', await file.arrayBuffer());
		expect(actions[0].value.archiveSha256).toBe(
			Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, '0')).join('')
		);
		for (const [index, offset] of [0n, 32768n].entries()) {
			const action = actions[index + 1];
			if (action?.case !== 'appendRestoreVerification') throw new Error('Missing append');
			expect(action.value.offset).toBe(offset);
			expect(action.value.data.length).toBe(index === 0 ? 32768 : 1);
			expect(action.value.data.every((byte) => byte === 7)).toBe(true);
		}
	});
	it('stops before sending on cancellation or an invalid file', async () => {
		let requests = 0;
		const client = new ConfigurationRestoreVerificationClient(async () => {
			requests++;
			throw new Error('network');
		});
		await expect(client.prepare(new File([], 'empty.zip'), () => {})).rejects.toThrow('size');
		await expect(
			client.prepare(new File(['zip'], 'valid.zip'), () => {
				throw new Error('session changed');
			})
		).rejects.toThrow('session changed');
		expect(requests).toBe(0);
	});
});
