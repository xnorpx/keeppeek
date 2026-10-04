import { create } from '@bufbuild/protobuf';
import { mount, unmount, tick } from 'svelte';
import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../../app.css';
import { StorageVolumeResultSchema, type StorageVolumeResult } from '$lib/proto/webrtc_pb';
import type { VolumeController } from '$lib/storage-volumes';
import MetadataControl from './MetadataControl.svelte';

const destination = '{secret:METADATA_VOLUME}';
function status(pending?: string, revision = 'status-revision') {
	return create(StorageVolumeResultSchema, {
		result: {
			case: 'metadata',
			value: {
				configurationRevision: revision,
				pendingVolumeId: pending,
				restartRequired: !!pending
			}
		}
	});
}
function preview() {
	return create(StorageVolumeResultSchema, {
		result: {
			case: 'metadataPreview',
			value: {
				destinationVolumeId: destination,
				previewToken: 'opaque-preview',
				configurationRevision: 'preview-revision',
				requiredBytes: 9007199254740993n,
				expiresInSeconds: 60,
				restartRequired: true,
				requiresDowntime: true
			}
		}
	});
}
function controller(send: VolumeController['storageVolumes']): VolumeController {
	return { storageVolumes: send, updateRuntimeConfiguration: vi.fn() };
}
async function chooseDestination() {
	await page.getByRole('combobox', { name: /^Metadata destination/ }).selectOptions(destination);
}

describe('metadata transfer review', () => {
	it('preserves secret IDs and exact bytes, and stages only after explicit confirmation', async () => {
		await page.viewport(320, 800);
		const send = vi
			.fn<VolumeController['storageVolumes']>()
			.mockResolvedValueOnce(status())
			.mockResolvedValueOnce(preview())
			.mockResolvedValueOnce(status(destination, 'confirmed-revision'));
		const view = await render(MetadataControl, {
			controller: controller(send),
			volumes: [destination]
		});
		await expect.element(page.getByText(/Current metadata location/)).toBeVisible();
		await chooseDestination();
		await expect
			.element(page.getByRole('button', { name: 'Confirm metadata move' }))
			.not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Preview metadata move' }).click();
		expect(send.mock.calls.map(([command]) => command.action.case)).toEqual([
			'metadata',
			'previewMetadata'
		]);
		expect(send.mock.calls[1][0].action).toMatchObject({
			case: 'previewMetadata',
			value: { destinationVolumeId: destination }
		});
		await expect.element(page.getByText(/9007199254740993/)).toBeVisible();
		await expect.element(page.getByText(/downtime/i)).toBeVisible();
		expect(view.container.scrollWidth).toBeLessThanOrEqual(320);
		await page.getByRole('button', { name: 'Confirm metadata move' }).click();
		expect(send.mock.calls[2][0].action).toMatchObject({
			case: 'confirmMetadata',
			value: {
				previewToken: 'opaque-preview',
				expectedConfigurationRevision: 'preview-revision'
			}
		});
		await expect.element(page.getByText(/Metadata move pending/)).toBeVisible();
	});

	it('restores a pending move on mount and cancels using the refreshed status revision', async () => {
		const send = vi
			.fn<VolumeController['storageVolumes']>()
			.mockResolvedValueOnce(status(destination, 'first-status'))
			.mockResolvedValueOnce(status(destination, 'refreshed-status'))
			.mockResolvedValueOnce(status(undefined, 'cancelled-status'));
		await render(MetadataControl, { controller: controller(send), volumes: [destination] });
		await expect.element(page.getByText(/Metadata move pending/)).toBeVisible();
		await page.getByRole('button', { name: 'Refresh metadata status' }).click();
		await page.getByRole('button', { name: 'Cancel pending metadata move' }).click();
		expect(send.mock.calls[2][0].action).toMatchObject({
			case: 'cancelMetadata',
			value: {
				expectedConfigurationRevision: 'refreshed-status'
			}
		});
		await expect.element(page.getByText(/Metadata move pending/)).not.toBeInTheDocument();
	});

	it('surfaces a stale confirmation without claiming a transfer was staged', async () => {
		const send = vi
			.fn<VolumeController['storageVolumes']>()
			.mockResolvedValueOnce(status())
			.mockResolvedValueOnce(preview())
			.mockRejectedValueOnce(new Error('Configuration changed; preview again'));
		await render(MetadataControl, { controller: controller(send), volumes: [destination] });
		await chooseDestination();
		await page.getByRole('button', { name: 'Preview metadata move' }).click();
		await page.getByRole('button', { name: 'Confirm metadata move' }).click();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('Configuration changed; preview again');
		await expect.element(page.getByText(/Metadata move pending/)).not.toBeInTheDocument();
	});

	it('disables mutations before and after a preview is loaded', async () => {
		const send = vi.fn<VolumeController['storageVolumes']>().mockResolvedValue(status());
		const view = await render(MetadataControl, {
			controller: controller(send),
			volumes: [destination],
			disabled: true
		});
		await expect
			.element(page.getByRole('button', { name: 'Preview metadata move' }))
			.toBeDisabled();
		await view.rerender({ disabled: false });
		await chooseDestination();
		send.mockResolvedValueOnce(preview());
		await page.getByRole('button', { name: 'Preview metadata move' }).click();
		await expect.element(page.getByRole('button', { name: 'Confirm metadata move' })).toBeVisible();
		await view.rerender({ disabled: true });
		await expect
			.element(page.getByRole('button', { name: 'Confirm metadata move' }))
			.toBeDisabled();
		expect(send.mock.calls.every(([command]) => command.action.case !== 'confirmMetadata')).toBe(
			true
		);
	});

	it('ignores an initial status response delivered after unmount', async () => {
		let resolve!: (result: StorageVolumeResult) => void;
		const response = new Promise<StorageVolumeResult>((done) => {
			resolve = done;
		});
		const send = vi.fn<VolumeController['storageVolumes']>().mockReturnValue(response);
		const target = document.createElement('div');
		document.body.append(target);
		const component = mount(MetadataControl, {
			target,
			props: { controller: controller(send), volumes: [destination] }
		});
		try {
			await tick();
			await expect.poll(() => send.mock.calls.length).toBe(1);
			await unmount(component);
			resolve(status(destination));
			await response;
			await tick();
			expect(target.childElementCount).toBe(0);
			expect(send).toHaveBeenCalledTimes(1);
		} finally {
			target.remove();
		}
	});
});

it('reports pending metadata independently when it is cancelled', async () => {
	const changed = vi.fn();
	const send = vi
		.fn<VolumeController['storageVolumes']>()
		.mockResolvedValueOnce(status(destination))
		.mockResolvedValueOnce(status());
	await render(MetadataControl, {
		controller: controller(send),
		volumes: [destination],
		onpendingchange: changed
	});
	await expect.poll(() => changed.mock.calls).toEqual([[true]]);
	await page.getByRole('button', { name: 'Cancel pending metadata move' }).click();
	await expect.poll(() => changed.mock.calls).toEqual([[true], [false]]);
});
