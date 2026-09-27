import { create } from '@bufbuild/protobuf';
import { mount, unmount, tick, type ComponentProps } from 'svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import {
	ConfigurationRestoreVerificationSchema,
	AdministratorVerificationSchema,
	ExternalAuthenticationSettingsSchema,
	ExternalAuthenticationProviderSchema
} from '$lib/proto/webrtc_pb';
import { RestoreRecordSchema, RestoreState } from '$lib/proto/backup_pb';
import type { AccessConnectionState } from '$lib/access';
import ConfigurationRestoreReview from './ConfigurationRestoreReview.svelte';
import BackupRestoreSection from './BackupRestoreSection.svelte';
import '../../app.css';

const mounted: Array<ReturnType<typeof mount>> = [];
const targets: HTMLElement[] = [];
afterEach(async () => {
	for (const component of mounted.splice(0)) await unmount(component);
	for (const target of targets.splice(0)) target.remove();
	vi.restoreAllMocks();
});

function fixture(requiresProof = true, backupLocal?: boolean) {
	const file = new File(['exact zip bytes'], 'config.zip');
	const preparation = create(ConfigurationRestoreVerificationSchema, {
		preparationId: 'preparation',
		archiveBytes: BigInt(file.size),
		receivedBytes: BigInt(file.size),
		expiresAtMs: BigInt(Date.now() + 60_000),
		ready: true,
		requiresAdministratorConfirmation: requiresProof
	});
	const state: AccessConnectionState = {
		status: 'authenticated',
		generation: 1,
		message: null,
		session: {
			id: 'parent',
			principalId: 'admin',
			displayName: 'Administrator',
			role: 'administrator',
			local: backupLocal ?? false,
			clientClassification: 'remote',
			createdAtMs: 0,
			lastActivityAtMs: 0,
			absoluteExpiresAtMs: Date.now() + 60_000,
			credentialExpiresAtMs: null
		}
	};
	const listeners = new Set<(state: AccessConnectionState) => void>();
	const proof = create(AdministratorVerificationSchema, {
		verificationId: 'proof',
		configurationPlanId: 'preparation',
		verified: true,
		expiresAtMs: preparation.expiresAtMs
	});
	const record = create(RestoreRecordSchema, { state: RestoreState.AWAITING_RESTART });
	const controller: ComponentProps<typeof BackupRestoreSection>['controller'] = {
		onCapabilities: (listener) => {
			listener(['keeppeek.backup.v1']);
			return () => {};
		},
		exportConfiguration: vi
			.fn()
			.mockResolvedValue({ blob: new Blob(['zip']), fileName: 'backup.zip' }),
		onAccessState: (listener) => {
			listeners.add(listener);
			listener(state);
			return () => {
				listeners.delete(listener);
			};
		},
		prepareConfigurationRestoreVerification: vi.fn().mockResolvedValue(preparation),
		getConfigurationRestoreVerification: vi.fn().mockResolvedValue(preparation),
		confirmConfigurationRestoreVerification: vi
			.fn()
			.mockResolvedValue({ ...preparation, confirmed: true }),
		prepareAdministratorVerification: vi.fn().mockResolvedValue(proof),
		getAdministratorVerification: vi.fn().mockResolvedValue(proof),
		verifyAdministratorBearer: vi.fn().mockResolvedValue(proof),
		applyExternalAuthentication: vi
			.fn()
			.mockRejectedValue(new Error('Must not apply a settings plan')),
		applyConfiguration: vi.fn().mockResolvedValue(record)
	};
	const target = document.createElement('div');
	document.body.append(target);
	targets.push(target);
	const onstaged = vi.fn();
	mounted.push(
		backupLocal === undefined
			? mount(ConfigurationRestoreReview, { target, props: { controller, file, onstaged } })
			: mount(BackupRestoreSection, { target, props: { controller, onrestart: () => {} } })
	);
	return { controller, file, preparation, record, onstaged, listeners, state };
}

async function inspect() {
	await page.getByRole('button', { name: 'Inspect restore archive' }).click();
	await expect
		.element(page.getByRole('heading', { name: 'Review configuration restore' }))
		.toBeVisible();
}

describe('remote configuration restore review', () => {
	it('keeps local restores on the ordinary guarded HTTP path', async () => {
		const { controller, file } = fixture(false, true);
		await page.getByLabelText('Configuration ZIP').upload(file);
		await page.getByRole('checkbox').click();
		await page.getByRole('button', { name: 'Apply configuration' }).click();
		await expect
			.element(page.getByRole('status'))
			.toHaveTextContent('Configuration staged. Restart required.');
		expect(controller.prepareConfigurationRestoreVerification).not.toHaveBeenCalled();
		expect(controller.confirmConfigurationRestoreVerification).not.toHaveBeenCalled();
		expect(await vi.mocked(controller.applyConfiguration).mock.calls[0][0].text()).toBe(
			await file.text()
		);
	});
	it('discards proof and aborts preparation when a different archive is selected', async () => {
		const { controller, file } = fixture(false, false);
		await page.getByLabelText('Configuration ZIP').upload(file);
		await inspect();
		const signal = vi.mocked(controller.prepareConfigurationRestoreVerification).mock.calls[0][1];
		await page.getByLabelText('Configuration ZIP').upload(new File(['another zip'], 'other.zip'));
		await expect
			.element(page.getByRole('heading', { name: 'Review configuration restore' }))
			.not.toBeInTheDocument();
		expect(signal.aborted).toBe(true);
		expect(controller.applyConfiguration).not.toHaveBeenCalled();
	});
	it('locks file selection during confirmation and never uploads after RTC loss', async () => {
		const { controller, file, listeners, state, preparation } = fixture(false, false);
		let resolve!: (value: typeof preparation) => void;
		vi.mocked(controller.confirmConfigurationRestoreVerification).mockImplementation(
			() =>
				new Promise((done) => {
					resolve = done;
				})
		);
		await page.getByLabelText('Configuration ZIP').upload(file);
		await inspect();
		await page.getByRole('checkbox').click();
		await page.getByRole('button', { name: 'Confirm and stage restore' }).click();
		await expect.element(page.getByLabelText('Configuration ZIP')).toBeDisabled();
		for (const listener of listeners) listener({ ...state, generation: 2 });
		resolve({ ...preparation, confirmed: true });
		await tick();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent(
				'The control session changed. Select the archive again on a live remote Administrator session.'
			);
		expect(controller.applyConfiguration).not.toHaveBeenCalled();
	});
	it('requires fresh proof and explicit confirmation before staging the exact inspected File', async () => {
		const { controller, file, onstaged, record } = fixture();
		await inspect();
		await expect
			.element(page.getByRole('button', { name: 'Confirm and stage restore' }))
			.toBeDisabled();
		await page.getByLabelText('Replacement Administrator access key').fill('replacement-key');
		await page.getByRole('button', { name: 'Verify replacement key' }).click();
		expect(controller.verifyAdministratorBearer).toHaveBeenCalledWith(
			'preparation',
			'replacement-key'
		);
		await expect
			.element(page.getByRole('button', { name: 'Confirm and stage restore' }))
			.toBeDisabled();
		await page.getByRole('checkbox').click();
		await page.getByRole('button', { name: 'Confirm and stage restore' }).click();
		expect(controller.confirmConfigurationRestoreVerification).toHaveBeenCalledWith(
			'preparation',
			'proof'
		);
		expect(vi.mocked(controller.applyConfiguration).mock.calls[0][0]).toBe(file);
		expect(controller.applyExternalAuthentication).not.toHaveBeenCalled();
		expect(onstaged).toHaveBeenCalledWith(record);
	});
	it('does not upload when the server declines confirmation', async () => {
		const { controller } = fixture(false);
		vi.mocked(controller.confirmConfigurationRestoreVerification).mockRejectedValue(
			new Error('stale revision')
		);
		await inspect();
		await page.getByRole('checkbox').click();
		await page.getByRole('button', { name: 'Confirm and stage restore' }).click();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent(
				'Restore was not confirmed as staged. Select the archive again and inspect it before retrying.'
			);
		expect(controller.applyConfiguration).not.toHaveBeenCalled();
	});
	it('invalidates pending preparation when the RTC generation changes', async () => {
		const { controller, listeners, state } = fixture(false);
		await inspect();
		for (const listener of listeners) listener({ ...state, generation: 2 });
		await tick();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent(
				'The control session changed. Select the archive again on a live remote Administrator session.'
			);
		expect(controller.applyConfiguration).not.toHaveBeenCalled();
		expect(
			vi.mocked(controller.prepareConfigurationRestoreVerification).mock.calls[0][1].aborted
		).toBe(true);
	});
	it('rejects expired preparation before proof or confirmation', async () => {
		const { preparation, controller } = fixture();
		preparation.expiresAtMs = 1n;
		await page.getByRole('button', { name: 'Inspect restore archive' }).click();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('Restore preparation expired. Prepare the archive again.');
		expect(controller.confirmConfigurationRestoreVerification).not.toHaveBeenCalled();
		expect(controller.applyConfiguration).not.toHaveBeenCalled();
	});
	it('selects providers from the inspected candidate, not current settings', async () => {
		const { preparation } = fixture();
		preparation.candidateAuthentication = create(ExternalAuthenticationSettingsSchema, {
			providers: [
				create(ExternalAuthenticationProviderSchema, {
					providerId: 'replacement',
					name: 'Replacement provider'
				})
			]
		});
		await inspect();
		await expect
			.element(page.getByRole('option', { name: 'Replacement provider' }))
			.toBeInTheDocument();
		await expect
			.element(page.getByLabelText('Replacement Administrator access key'))
			.not.toBeInTheDocument();
	});
	it('aborts and unsubscribes on unmount', async () => {
		const { controller, listeners } = fixture(false);
		await inspect();
		await unmount(mounted.pop()!);
		expect(listeners.size).toBe(0);
		expect(
			vi.mocked(controller.prepareConfigurationRestoreVerification).mock.calls[0][1].aborted
		).toBe(true);
	});
});
