import { page } from 'vitest/browser';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { flushSync, tick } from 'svelte';
import { create } from '@bufbuild/protobuf';
import {
	RecordingMaintenanceClient,
	RECORDING_MAINTENANCE_CAPABILITY
} from '$lib/control-client-maintenance';
import { CapabilityState } from '$lib/capability-state.svelte';
import {
	RecordingDeletionJobSchema,
	RecordingDeletionJobListSchema,
	RecordingDeletionStatus,
	type RecordingDeletionJob,
	type Request,
	type Ok
} from '$lib/proto/webrtc_pb';
import '../../../app.css';
import Maintenance from './+page.svelte';

const context = vi.hoisted(() => ({ control: vi.fn(), capabilities: vi.fn() }));
vi.mock('$lib/control-context', () => ({ useControlClient: context.control }));
vi.mock('$lib/capability-context', () => ({
	useCapabilityState: context.capabilities
}));
vi.mock('$app/state', () => ({
	page: { url: new URL('http://localhost/recordings/maintenance') }
}));
vi.mock('$app/paths', () => ({ resolve: (value: string) => value }));

function job(status: RecordingDeletionStatus): RecordingDeletionJob {
	return create(RecordingDeletionJobSchema, {
		jobId: '1'.repeat(32),
		status,
		bytes: 64n,
		revision: 1n,
		createdAtMs: 1_000n,
		expiresAtMs: BigInt(Date.now() + 600_000),
		requiredConfirmationText: 'DELETE 1',
		objects: [{ recordingId: 'recording', startMs: 1_000n, endMs: 2_000n, bytes: 64n, status }]
	});
}

function controller(value: RecordingDeletionJob) {
	const capabilities = new CapabilityState([RECORDING_MAINTENANCE_CAPABILITY]);
	context.capabilities.mockReturnValue(capabilities);
	let notifyAccess: (value: { session: { role: string; principalId: string } }) => void = () => {};
	const send = vi.fn(async (command: Request['command']): Promise<Ok['result']> => {
		if (command.case !== 'recordingMaintenanceCommand') throw new Error('Unexpected command');
		if (command.value.action.case === 'list') {
			return {
				case: 'recordingDeletionJobs',
				value: create(RecordingDeletionJobListSchema, { jobs: value.jobId ? [value] : [] })
			};
		}
		return { case: 'recordingDeletionJob', value };
	});
	context.control.mockReturnValue({
		recordingMaintenance: new RecordingMaintenanceClient(send),
		getCameras: async () => [{ id: 'front', name: 'Front' }],
		onAccessState: (listener: typeof notifyAccess) => {
			notifyAccess = listener;
			listener({ session: { role: 'administrator', principalId: 'local' } });
			return () => {};
		}
	});
	return Object.assign(send, {
		capabilities,
		access: (role: string) => notifyAccess({ session: { role, principalId: 'local' } })
	});
}

beforeEach(() => vi.clearAllMocks());

describe('Recording maintenance recovery', () => {
	it('keeps an explicitly dismissed pending refresh closed', async () => {
		const prepared = job(RecordingDeletionStatus.PREPARED);
		prepared.confirmationNonce = '2'.repeat(64);
		const send = controller(prepared);
		const originalSend = send.getMockImplementation()!;
		const deferred = Promise.withResolvers<Ok['result']>();
		let previews = 0;
		send.mockImplementation((command) => {
			if (command.case === 'recordingMaintenanceCommand') {
				if (command.value.action.case === 'confirm')
					return Promise.reject(new Error('Catalog changed'));
				if (command.value.action.case === 'preview' && ++previews > 1) return deferred.promise;
			}
			return originalSend(command);
		});
		await render(Maintenance);
		await page.getByRole('button', { name: 'Preview deletion' }).click();
		const dialog = page.getByRole('dialog');
		await dialog.getByRole('textbox', { name: 'Type DELETE 1' }).fill('DELETE 1');
		await dialog.getByRole('button', { name: 'Delete permanently' }).click();
		await dialog.getByRole('button', { name: 'Refresh preview' }).click();
		await dialog.getByRole('button', { name: 'Keep recordings' }).click();
		deferred.resolve({ case: 'recordingDeletionJob', value: prepared });
		await deferred.promise;
		await tick();
		await tick();
		await expect.element(document.querySelector('dialog')!).not.toHaveAttribute('open');
	});

	it('invalidates confirmation when capability is lost and restored in one batch', async () => {
		const prepared = job(RecordingDeletionStatus.PREPARED);
		prepared.confirmationNonce = '2'.repeat(64);
		const send = controller(prepared);
		await render(Maintenance);
		await page.getByRole('button', { name: 'Preview deletion' }).click();
		await page.getByRole('dialog').getByRole('textbox', { name: 'Type DELETE 1' }).fill('DELETE 1');
		flushSync(() => {
			send.capabilities.updateAdvertised([]);
			send.capabilities.updateAdvertised([RECORDING_MAINTENANCE_CAPABILITY]);
		});
		await expect.element(document.querySelector('dialog')!).not.toHaveAttribute('open');
	});

	it('does not accept confirmation text entered before a refreshed preview arrives', async () => {
		const prepared = job(RecordingDeletionStatus.PREPARED);
		prepared.confirmationNonce = '2'.repeat(64);
		const send = controller(prepared);
		const originalSend = send.getMockImplementation()!;
		const deferred = Promise.withResolvers<Ok['result']>();
		let previews = 0;
		send.mockImplementation((command) => {
			if (command.case === 'recordingMaintenanceCommand') {
				if (command.value.action.case === 'confirm')
					return Promise.reject(new Error('Catalog changed'));
				if (command.value.action.case === 'preview' && ++previews > 1) return deferred.promise;
			}
			return originalSend(command);
		});
		await render(Maintenance);
		await page.getByRole('button', { name: 'Preview deletion' }).click();
		const dialog = page.getByRole('dialog');
		await dialog.getByRole('textbox', { name: 'Type DELETE 1' }).fill('DELETE 1');
		await dialog.getByRole('button', { name: 'Delete permanently' }).click();
		await dialog.getByRole('button', { name: 'Refresh preview' }).click();
		await expect.element(dialog.getByRole('textbox', { name: 'Type DELETE 1' })).toBeDisabled();
		deferred.resolve({
			case: 'recordingDeletionJob',
			value: create(RecordingDeletionJobSchema, { ...prepared, revision: 2n })
		});
		await expect.element(dialog.getByRole('textbox', { name: 'Type DELETE 1' })).toBeEnabled();
		await expect.element(dialog.getByRole('textbox', { name: 'Type DELETE 1' })).toHaveValue('');
		await expect.element(dialog.getByRole('button', { name: 'Delete permanently' })).toBeDisabled();
	});

	it('does not let an old confirmation reply dismiss a newer review', async () => {
		const prepared = job(RecordingDeletionStatus.PREPARED);
		prepared.confirmationNonce = '2'.repeat(64);
		const send = controller(prepared);
		const originalSend = send.getMockImplementation()!;
		const deferred = Promise.withResolvers<Ok['result']>();
		send.mockImplementation((command) =>
			command.case === 'recordingMaintenanceCommand' && command.value.action.case === 'confirm'
				? deferred.promise
				: originalSend(command)
		);
		await render(Maintenance);
		await page.getByRole('button', { name: 'Preview deletion' }).click();
		const dialog = page.getByRole('dialog');
		await dialog.getByRole('textbox', { name: 'Type DELETE 1' }).fill('DELETE 1');
		await dialog.getByRole('button', { name: 'Delete permanently' }).click();
		flushSync(() => send.access('user'));
		flushSync(() => send.access('administrator'));
		await page.getByRole('button', { name: 'Preview deletion' }).click();
		await dialog.getByRole('textbox', { name: 'Type DELETE 1' }).fill('DELETE 1');
		deferred.resolve({ case: 'recordingDeletionJob', value: job(RecordingDeletionStatus.DELETED) });
		await deferred.promise;
		await tick();
		await tick();
		await expect.element(dialog).toBeVisible();
		await expect
			.element(dialog.getByRole('textbox', { name: 'Type DELETE 1' }))
			.toHaveValue('DELETE 1');
	});

	it.each(['role', 'capability'])('discards reviewed confirmation after %s loss', async (loss) => {
		const prepared = job(RecordingDeletionStatus.PREPARED);
		prepared.confirmationNonce = '2'.repeat(64);
		const send = controller(prepared);
		await render(Maintenance);
		await page.getByRole('button', { name: 'Preview deletion' }).click();
		await page.getByRole('dialog').getByRole('textbox', { name: 'Type DELETE 1' }).fill('DELETE 1');
		flushSync(() => {
			if (loss === 'role') send.access('user');
			else send.capabilities.updateAdvertised([]);
		});
		await expect.element(document.querySelector('dialog')!).not.toHaveAttribute('open');
		flushSync(() => {
			if (loss === 'role') send.access('administrator');
			else send.capabilities.updateAdvertised([RECORDING_MAINTENANCE_CAPABILITY]);
		});
		await page.getByRole('button', { name: 'Preview deletion' }).click();
		await expect
			.element(page.getByRole('dialog').getByRole('textbox', { name: 'Type DELETE 1' }))
			.toHaveValue('');
		await expect
			.element(page.getByRole('dialog').getByRole('button', { name: 'Delete permanently' }))
			.toBeDisabled();
	});

	it('ignores a preview arriving after access was lost and restored', async () => {
		const prepared = job(RecordingDeletionStatus.PREPARED);
		prepared.confirmationNonce = '2'.repeat(64);
		const send = controller(prepared);
		const originalSend = send.getMockImplementation()!;
		const deferred = Promise.withResolvers<Ok['result']>();
		send.mockImplementation((command) =>
			command.case === 'recordingMaintenanceCommand' && command.value.action.case === 'preview'
				? deferred.promise
				: originalSend(command)
		);
		await render(Maintenance);
		await page.getByRole('button', { name: 'Preview deletion' }).click();
		flushSync(() => send.access('user'));
		flushSync(() => send.access('administrator'));
		deferred.resolve({ case: 'recordingDeletionJob', value: prepared });
		await deferred.promise;
		await tick();
		await tick();
		await expect.element(document.querySelector('dialog')!).not.toHaveAttribute('open');
		expect(
			send.mock.calls.filter(
				([command]) =>
					command.case === 'recordingMaintenanceCommand' && command.value.action.case === 'confirm'
			)
		).toHaveLength(0);
	});

	it('regenerates a stale preview and requires confirmation to be typed again', async () => {
		const prepared = job(RecordingDeletionStatus.PREPARED);
		prepared.confirmationNonce = '2'.repeat(64);
		const send = controller(prepared);
		const defaultSend = send.getMockImplementation()!;
		let previews = 0;
		send.mockImplementation(async (command) => {
			if (command.case === 'recordingMaintenanceCommand') {
				if (command.value.action.case === 'confirm')
					throw new Error('Catalog changed; preview again.');
				if (command.value.action.case === 'preview') {
					previews += 1;
					return {
						case: 'recordingDeletionJob',
						value: create(RecordingDeletionJobSchema, { ...prepared, revision: BigInt(previews) })
					};
				}
			}
			return defaultSend(command);
		});
		await render(Maintenance);
		await page.getByRole('button', { name: 'Preview deletion' }).click();
		const dialog = page.getByRole('dialog');
		await dialog.getByRole('textbox', { name: 'Type DELETE 1' }).fill('DELETE 1');
		await dialog.getByRole('button', { name: 'Delete permanently' }).click();
		await expect.element(dialog.getByRole('alert')).toHaveTextContent('Catalog changed');
		await expect.element(dialog.getByRole('button', { name: 'Delete permanently' })).toBeDisabled();
		await dialog.getByRole('button', { name: 'Refresh preview' }).click();
		await expect.element(dialog.getByRole('textbox', { name: 'Type DELETE 1' })).toHaveValue('');
		await expect.element(dialog.getByRole('button', { name: 'Delete permanently' })).toBeDisabled();
		expect(previews).toBe(2);
	});

	it('offers retry for cancelled jobs that retain failed objects', async () => {
		const pending = job(RecordingDeletionStatus.CANCELLED);
		pending.cancelled = true;
		pending.failedCount = 1;
		pending.objects[0].status = RecordingDeletionStatus.FAILED;
		const send = controller(pending);
		await render(Maintenance);
		await page.getByRole('button', { name: /^111111111111/ }).click();
		await expect.element(page.getByRole('button', { name: 'Retry failed objects' })).toBeVisible();
		await page.getByRole('button', { name: 'Retry failed objects' }).click();
		expect(
			send.mock.calls.some(
				([command]) =>
					command.case === 'recordingMaintenanceCommand' && command.value.action.case === 'retry'
			)
		).toBe(true);
	});

	it('does not offer job commands for blocked previews without a durable job', async () => {
		const blocked = job(RecordingDeletionStatus.BLOCKED);
		blocked.jobId = '';
		blocked.objects[0].protected = true;
		blocked.objects[0].error = 'Recording is protected';
		controller(blocked);
		await render(Maintenance);
		await page.getByRole('button', { name: 'Preview deletion' }).click();
		await expect
			.element(
				page
					.getByRole('region', { name: 'Deletion progress' })
					.getByText('Recording is protected', { exact: true })
			)
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Refresh job' })).not.toBeInTheDocument();
	});
});
