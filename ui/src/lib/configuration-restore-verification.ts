import { create } from '@bufbuild/protobuf';
import { maximumConfigurationArchiveBytes as maximumArchiveBytes } from './backup-http-client';
import {
	ServerCommandSchema,
	ExternalAuthenticationCommandSchema,
	BeginConfigurationRestoreVerificationSchema,
	AppendConfigurationRestoreVerificationSchema,
	GetConfigurationRestoreVerificationSchema,
	ConfirmConfigurationRestoreVerificationSchema,
	AdministratorConfirmationSchema,
	type ExternalAuthenticationCommand,
	type ConfigurationRestoreVerification,
	type Request,
	type Ok
} from './proto/webrtc_pb';

const chunkBytes = 32768;

export class ConfigurationRestoreVerificationClient {
	constructor(private readonly send: (command: Request['command']) => Promise<Ok['result']>) {}

	private async command(action: ExternalAuthenticationCommand['action']) {
		const result = await this.send({
			case: 'serverCommand',
			value: create(ServerCommandSchema, {
				action: {
					case: 'externalAuthentication',
					value: create(ExternalAuthenticationCommandSchema, { action })
				}
			})
		});
		if (
			result.case !== 'externalAuthenticationResult' ||
			result.value.result.case !== 'restoreVerification'
		)
			throw new Error('Unexpected restore verification response.');
		return result.value.result.value;
	}

	begin(archiveBytes: bigint, archiveSha256: string) {
		if (
			archiveBytes <= 0n ||
			archiveBytes > BigInt(maximumArchiveBytes) ||
			!/^[a-f0-9]{64}$/.test(archiveSha256)
		)
			throw new Error('Invalid restore archive size or checksum.');
		return this.command({
			case: 'beginRestoreVerification',
			value: create(BeginConfigurationRestoreVerificationSchema, { archiveBytes, archiveSha256 })
		});
	}

	append(preparationId: string, offset: bigint, data: Uint8Array) {
		if (!preparationId || offset < 0n || data.length === 0 || data.length > chunkBytes)
			throw new Error('Invalid restore upload chunk.');
		return this.command({
			case: 'appendRestoreVerification',
			value: create(AppendConfigurationRestoreVerificationSchema, { preparationId, offset, data })
		});
	}

	get(preparationId: string) {
		return this.command({
			case: 'getRestoreVerification',
			value: create(GetConfigurationRestoreVerificationSchema, { preparationId })
		});
	}

	confirm(preparationId: string, verificationId?: string) {
		return this.command({
			case: 'confirmRestoreVerification',
			value: create(ConfirmConfigurationRestoreVerificationSchema, {
				preparationId,
				confirm: true,
				administratorConfirmation: verificationId
					? create(AdministratorConfirmationSchema, { verificationId, confirm: true })
					: undefined
			})
		});
	}

	async prepare(file: File, guard: () => void, onprogress?: (receivedBytes: number) => void) {
		if (file.size === 0 || file.size > maximumArchiveBytes)
			throw new Error('Invalid restore archive size (1 GiB maximum).');
		guard();
		// ponytail: Web Crypto hashes at most the existing 1 GiB archive limit in memory; use a streaming hash if this becomes a measured constraint.
		const digest = await crypto.subtle.digest('SHA-256', await file.arrayBuffer());
		guard();
		const checksum = Array.from(new Uint8Array(digest), (byte) =>
			byte.toString(16).padStart(2, '0')
		).join('');
		let result = await this.begin(BigInt(file.size), checksum);
		guard();
		const id = result.preparationId;
		validateRestorePreparation(result, id, file.size, 0);
		for (let offset = 0; offset < file.size; offset += chunkBytes) {
			guard();
			validateRestorePreparation(result, id, file.size, offset);
			const data = new Uint8Array(await file.slice(offset, offset + chunkBytes).arrayBuffer());
			guard();
			result = await this.append(id, BigInt(offset), data);
			guard();
			validateRestorePreparation(result, id, file.size, offset + data.length);
			onprogress?.(offset + data.length);
		}
		result = await this.get(id);
		guard();
		validateRestorePreparation(result, id, file.size, file.size);
		if (!result.ready)
			throw new Error('Restore inspection is not ready. Prepare the archive again.');
		return result;
	}
}

export function validateRestorePreparation(
	result: ConfigurationRestoreVerification,
	id: string,
	size: number,
	received: number
): void {
	if (
		!id ||
		result.preparationId !== id ||
		result.archiveBytes !== BigInt(size) ||
		result.receivedBytes !== BigInt(received)
	)
		throw new Error('Restore preparation no longer matches this archive.');
	if (Number(result.expiresAtMs) <= Date.now())
		throw new Error('Restore preparation expired. Prepare the archive again.');
}
