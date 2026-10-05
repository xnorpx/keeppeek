import { create } from '@bufbuild/protobuf';
import { page } from 'vitest/browser';
import { expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { StorageVolumeResultSchema } from '$lib/proto/webrtc_pb';
import type { VolumeController } from '$lib/storage-volumes';
import MoveJobs from './MoveJobs.svelte';

it('cancels an admitted move and does not offer cancellation after publication', async () => {
	const send = vi.fn<VolumeController['storageVolumes']>().mockImplementation(async (command) => {
		if (command.action.case === 'cancelMove')
			return create(StorageVolumeResultSchema, {
				result: {
					case: 'job',
					value: { jobId: 'pending', phase: 'reserved', cancellationRequested: true }
				}
			});
		return create(StorageVolumeResultSchema, {
			result: {
				case: 'jobs',
				value: {
					jobs: [
						{ jobId: 'pending', phase: 'reserved' },
						{ jobId: 'published', phase: 'published' }
					]
				}
			}
		});
	});
	await render(MoveJobs, {
		controller: { storageVolumes: send, updateRuntimeConfiguration: vi.fn() }
	});
	await page.getByRole('button', { name: 'Refresh move jobs' }).click();
	await expect
		.element(page.getByRole('button', { name: 'Cancel move published' }))
		.not.toBeInTheDocument();
	await page.getByRole('button', { name: 'Cancel move pending' }).click();
	expect(send.mock.calls[1][0].action).toMatchObject({
		case: 'cancelMove',
		value: { jobId: 'pending' }
	});
	await expect.element(page.getByRole('button', { name: 'Cancel move pending' })).toBeDisabled();
});
