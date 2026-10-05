import { create } from '@bufbuild/protobuf';
import { page } from 'vitest/browser';
import { expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import {
	StorageVolumeResultSchema,
	StorageObjectKind,
	StorageVolumeRole
} from '$lib/proto/webrtc_pb';
import type { VolumeController } from '$lib/storage-volumes';
import VolumeOperations from './VolumeOperations.svelte';

it('requires a preview and confirms only its token and configuration revision', async () => {
	const object = { kind: StorageObjectKind.EXPORT, id: 'artifact-one' };
	const source = { object, volumeId: 'primary', bytes: 9007199254740993n };
	const send = vi.fn<VolumeController['storageVolumes']>().mockImplementation(async (command) => {
		switch (command.action.case) {
			case 'objects':
				return create(StorageVolumeResultSchema, {
					result: { case: 'objects', value: { objects: [source] } }
				});
			case 'previewMove':
				return create(StorageVolumeResultSchema, {
					result: {
						case: 'preview',
						value: {
							source,
							destinationVolumeId: 'secondary',
							previewToken: 'token-one',
							configurationRevision: 'revision-one',
							expiresInSeconds: 60
						}
					}
				});
			case 'confirmMove':
				return create(StorageVolumeResultSchema, {
					result: { case: 'job', value: { jobId: 'move-one', phase: 'pending' } }
				});
			default:
				throw new Error('Unexpected command');
		}
	});
	const controller: VolumeController = {
		storageVolumes: send,
		updateRuntimeConfiguration: vi.fn()
	};
	await render(VolumeOperations, { controller, volumes: ['primary', 'secondary'] });
	await page.getByRole('combobox', { name: /^Source volume/ }).selectOptions('primary');
	await page.getByRole('button', { name: 'Load stored objects' }).click();
	await page
		.getByRole('combobox', { name: /^Stored object/ })
		.selectOptions(page.getByRole('option', { name: /artifact-one/ }));
	await page.getByRole('combobox', { name: /^Destination volume/ }).selectOptions('secondary');
	await expect
		.element(page.getByRole('button', { name: 'Confirm this move' }))
		.not.toBeInTheDocument();
	await page.getByRole('button', { name: 'Preview move' }).click();
	expect(send.mock.calls[1][0].action).toMatchObject({
		case: 'previewMove',
		value: { object, destinationVolumeId: 'secondary', role: StorageVolumeRole.EXPORT }
	});
	await page.getByRole('button', { name: 'Confirm this move' }).click();
	expect(send.mock.calls[2][0].action).toMatchObject({
		case: 'confirmMove',
		value: { previewToken: 'token-one', expectedConfigurationRevision: 'revision-one' }
	});
	await expect.element(page.getByText(/Move move-one: pending/)).toBeVisible();
});

function batchController() {
	const objects = Array.from({ length: 18 }, (_, index) => ({
		object: { kind: StorageObjectKind.EXPORT, id: `artifact-${index}` },
		volumeId: 'primary',
		bytes: 9007199254740993n
	}));
	let refused = false;
	const send = vi
		.fn<VolumeController['storageVolumes']>()
		.mockImplementation(async ({ action }) => {
			if (action.case === 'objects')
				return create(StorageVolumeResultSchema, {
					result: { case: 'objects', value: { objects } }
				});
			if (action.case === 'previewMove') {
				const source = objects.find((item) => item.object.id === action.value.object?.id)!;
				return create(StorageVolumeResultSchema, {
					result: {
						case: 'preview',
						value: {
							source,
							destinationVolumeId: 'secondary',
							previewToken: source.object.id,
							jobId: source.object.id,
							configurationRevision: 'revision',
							expiresInSeconds: 300
						}
					}
				});
			}
			if (action.case === 'confirmMove') {
				const id = action.value.previewToken;
				if (id === 'artifact-0') throw new Error('Reply lost');
				if (id === 'artifact-1' && !refused) {
					refused = true;
					throw new Error('Destination offline');
				}
				return create(StorageVolumeResultSchema, {
					result: { case: 'job', value: { jobId: id, phase: 'reserved' } }
				});
			}
			if (action.case === 'getMove' && action.value.jobId === 'artifact-0') {
				return create(StorageVolumeResultSchema, {
					result: { case: 'job', value: { jobId: 'artifact-0', phase: 'reserved' } }
				});
			}
			throw new Error('Job unavailable');
		});
	return { send, controller: { storageVolumes: send, updateRuntimeConfiguration: vi.fn() } };
}

it('bounds batches, preserves exact totals and resolves lost replies before retrying remaining jobs', async () => {
	const { send, controller } = batchController();
	await render(VolumeOperations, { controller, volumes: ['primary', 'secondary'] });
	await page.getByRole('combobox', { name: /^Source volume/ }).selectOptions('primary');
	await page.getByRole('button', { name: 'Load stored objects' }).click();
	await page.getByRole('combobox', { name: /^Destination volume/ }).selectOptions('secondary');
	await page.getByRole('button', { name: 'Preview next 16 objects' }).click();
	await expect.element(page.getByText(/16 files, 144115188075855888 bytes/)).toBeVisible();
	expect(send.mock.calls.filter(([request]) => request.action.case === 'previewMove')).toHaveLength(
		16
	);
	expect(send.mock.calls.filter(([request]) => request.action.case === 'confirmMove')).toHaveLength(
		0
	);
	await page.getByRole('button', { name: 'Confirm batch of 16 moves' }).click();
	await expect.element(page.getByText(/artifact-1: Destination offline/)).toBeVisible();
	expect(send.mock.calls.filter(([request]) => request.action.case === 'confirmMove')).toHaveLength(
		2
	);
	await page.getByRole('button', { name: 'Confirm batch of 15 moves' }).click();
	await expect.element(page.getByText(/artifact-15: reserved/)).toBeVisible();
	expect(
		send.mock.calls.filter(
			([request]) =>
				request.action.case === 'confirmMove' && request.action.value.previewToken === 'artifact-0'
		)
	).toHaveLength(1);
	await expect.element(page.getByRole('button', { name: 'Preview next 2 objects' })).toBeVisible();
});

it('stops batch dispatch after an in-flight confirmation fails', async () => {
	const { send, controller } = batchController();
	const respond = send.getMockImplementation()!;
	let rejectConfirmation: ((reason: Error) => void) | undefined;
	send.mockImplementation((request) =>
		request.action.case === 'confirmMove'
			? new Promise((_resolve, reject) => {
					rejectConfirmation = reject;
				})
			: respond(request)
	);
	await render(VolumeOperations, { controller, volumes: ['primary', 'secondary'] });
	await page.getByRole('combobox', { name: /^Source volume/ }).selectOptions('primary');
	await page.getByRole('button', { name: 'Load stored objects' }).click();
	await page.getByRole('combobox', { name: /^Destination volume/ }).selectOptions('secondary');
	await page.getByRole('button', { name: 'Preview next 16 objects' }).click();
	await page.getByRole('button', { name: 'Confirm batch of 16 moves' }).click();
	await expect.poll(() => rejectConfirmation).toBeDefined();
	await page.getByRole('button', { name: 'Stop after current request' }).click();
	rejectConfirmation!(new Error('Reply lost after stop'));
	await expect.element(page.getByText(/Reply lost after stop/)).toBeVisible();
	expect(send.mock.calls.filter(([request]) => request.action.case === 'confirmMove')).toHaveLength(
		1
	);
	expect(send.mock.calls.filter(([request]) => request.action.case === 'getMove')).toHaveLength(0);
});
