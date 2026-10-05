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
const legacyRecording = { kind: StorageObjectKind.RECORDING, id: 'legacy-one' };
const adoptionNotice =
	'Confirmation adopts this file into managed storage. Cancelling the transfer keeps it managed at its current location.';

function legacyController() {
	const send = vi.fn<VolumeController['storageVolumes']>().mockImplementation(async (command) => {
		switch (command.action.case) {
			case 'legacyObjects':
				return create(StorageVolumeResultSchema, {
					result: {
						case: 'legacyObjects',
						value: command.action.value.after
							? { objects: [{ object: { ...legacyRecording, id: 'legacy-two' }, revision: 12n }] }
							: {
									objects: [{ object: legacyRecording, revision: 11n }],
									nextAfter: legacyRecording
								}
					}
				});
			case 'previewMove':
				return create(StorageVolumeResultSchema, {
					result: {
						case: 'preview',
						value: {
							source: { object: legacyRecording, volumeId: 'legacy-archive', bytes: 1234n },
							destinationVolumeId: 'secondary',
							adoptsLegacy: true,
							previewToken: 'legacy-token-exact',
							configurationRevision: 'legacy-revision-exact',
							expiresInSeconds: 60
						}
					}
				});
			case 'confirmMove':
				return create(StorageVolumeResultSchema, {
					result: { case: 'job', value: { jobId: 'legacy-move', phase: 'pending' } }
				});
			default:
				throw new Error(`Unexpected action: ${command.action.case}`);
		}
	});
	const controller: VolumeController = {
		storageVolumes: send,
		updateRuntimeConfiguration: vi.fn()
	};
	return { send, controller };
}

async function loadLegacySource(controller: VolumeController) {
	await render(VolumeOperations, { controller, volumes: ['secondary'] });
	await expect
		.element(page.getByRole('option', { name: 'Legacy recordings', exact: true }))
		.toBeInTheDocument();
	await page.getByRole('combobox', { name: /^Source volume/ }).selectOptions('legacy:recordings');
	await page.getByRole('button', { name: 'Load stored objects' }).click();
}

it('discloses legacy adoption and confirms exactly the preview token and revision', async () => {
	const { send, controller } = legacyController();
	await loadLegacySource(controller);
	await expect
		.element(page.getByRole('option', { name: /legacy-one.*verify size in preview/ }))
		.toBeInTheDocument();
	await expect
		.element(page.getByRole('option', { name: /legacy-one.*0 bytes/ }))
		.not.toBeInTheDocument();
	await page
		.getByRole('combobox', { name: /^Stored object/ })
		.selectOptions(page.getByRole('option', { name: /legacy-one/ }));
	await page.getByRole('combobox', { name: /^Destination volume/ }).selectOptions('secondary');
	await expect
		.element(page.getByRole('button', { name: 'Confirm this move' }))
		.not.toBeInTheDocument();
	await page.getByRole('button', { name: 'Preview move' }).click();
	expect(send.mock.calls[1][0].action).toMatchObject({
		case: 'previewMove',
		value: {
			object: legacyRecording,
			destinationVolumeId: 'secondary',
			role: StorageVolumeRole.ARCHIVE
		}
	});
	await expect.element(page.getByText(adoptionNotice, { exact: true })).toBeVisible();
	expect(send.mock.calls.some(([command]) => command.action.case === 'confirmMove')).toBe(false);
	await page.getByRole('button', { name: 'Confirm this move' }).click();
	expect(send.mock.calls[2][0].action).toMatchObject({
		case: 'confirmMove',
		value: {
			previewToken: 'legacy-token-exact',
			expectedConfigurationRevision: 'legacy-revision-exact'
		}
	});
	await expect.element(page.getByText(/Move legacy-move: pending/)).toBeVisible();
});

it('pages legacy references with the legacy cursor rather than a named-volume request', async () => {
	const { send, controller } = legacyController();
	await loadLegacySource(controller);
	expect(send.mock.calls[0][0].action).toMatchObject({ case: 'legacyObjects' });
	await page.getByRole('button', { name: 'Next stored objects' }).click();
	expect(send.mock.calls[1][0].action).toMatchObject({
		case: 'legacyObjects',
		value: { after: legacyRecording }
	});
	await expect
		.element(page.getByRole('option', { name: /legacy-two.*verify size in preview/ }))
		.toBeInTheDocument();
	await expect
		.element(page.getByRole('button', { name: 'Next stored objects' }))
		.not.toBeInTheDocument();
	expect(send.mock.calls.some(([command]) => command.action.case === 'objects')).toBe(false);
});
