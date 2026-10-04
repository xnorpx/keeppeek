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
