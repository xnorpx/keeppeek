import { create } from '@bufbuild/protobuf';
import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { ArchiveController } from '$lib/archive';
import { StorageObjectKind, StorageVolumeResultSchema } from '$lib/proto/webrtc_pb';
import ArchiveCopy from './ArchiveCopy.svelte';

describe('archive copy identity failure', () => {
	it('shows recovery guidance without sending a command when secure randomness is unavailable', async () => {
		const control = {
			archive: vi.fn<ArchiveController['archive']>(),
			storageVolumes: vi
				.fn<ArchiveController['storageVolumes']>()
				.mockImplementation(async (request) =>
					create(StorageVolumeResultSchema, {
						result:
							request.action.case === 'list'
								? { case: 'volumes', value: { volumes: [{ volumeId: 'primary' }] } }
								: {
										case: 'objects',
										value: {
											objects: [{ object: { id: 'clip', kind: StorageObjectKind.RECORDING } }]
										}
									}
					})
				),
			getRuntimeConfiguration: vi.fn<ArchiveController['getRuntimeConfiguration']>(),
			updateRuntimeConfiguration: vi.fn<ArchiveController['updateRuntimeConfiguration']>()
		};
		await render(ArchiveCopy, { controller: control, onjob: vi.fn() });
		await page.getByRole('button', { name: 'Load archive source volumes' }).click();
		await page.getByLabelText('Source volume', { exact: true }).selectOptions('primary');
		await page.getByRole('button', { name: 'Load eligible media' }).click();
		await page
			.getByLabelText('Media object', { exact: true })
			.selectOptions(`${StorageObjectKind.RECORDING}:clip`);
		vi.stubGlobal('crypto', {});
		try {
			const button = page.getByRole('button', { name: 'Queue archive copy' }).element();
			if (!(button instanceof HTMLButtonElement)) throw new Error('Expected archive copy button.');
			button.click();
			await expect
				.element(page.getByRole('alert'))
				.toHaveTextContent(
					'Secure random IDs are unavailable. Use HTTPS or localhost, then retry. No archive copy was queued.'
				);
			expect(control.archive).not.toHaveBeenCalled();
		} finally {
			vi.unstubAllGlobals();
		}
	});
});
