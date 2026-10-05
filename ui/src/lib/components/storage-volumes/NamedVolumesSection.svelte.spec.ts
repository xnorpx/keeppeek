import { create } from '@bufbuild/protobuf';
import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../../app.css';
import {
	StorageVolumeConfigurationSchema,
	StorageVolumeResultSchema,
	StorageVolumeRole,
	StorageVolumeState
} from '$lib/proto/webrtc_pb';
import type { SanitizedConfig } from '$lib/types';
import type { VolumeController } from '$lib/storage-volumes';
import NamedVolumesSection from './NamedVolumesSection.svelte';
import VolumeDraftEditor from './VolumeDraftEditor.svelte';

function fixture(): SanitizedConfig {
	return {
		host: 'localhost',
		port: 3000,
		camera_count: 0,
		configuration_revision: 'revision-one',
		storage: {
			medium_term_path: '/legacy',
			long_term_path: '/legacy',
			recording_catalog_path: '/catalog',
			event_thumbnail_path: '/images',
			event_thumbnail_max_mb: 10,
			short_term_secs: 90,
			medium_term_secs: 1200,
			flush_interval_secs: 60,
			write_buffer_bytes: 8192,
			long_term_max_gb: 100,
			named_volumes: create(StorageVolumeConfigurationSchema, {
				volumes: [
					{
						id: 'archive',
						root: '{secret:ROOT}',
						roles: [StorageVolumeRole.ARCHIVE],
						state: StorageVolumeState.DISABLED,
						capacityBytes: 9007199254740993n
					}
				]
			})
		},
		recording_estimate: {
			estimated_bitrate_bps: 0,
			bytes_per_day: 0,
			known_streams: 0,
			unknown_streams: 0,
			estimated_retention_days: null
		}
	};
}

describe('named volume settings', () => {
	it('confirms operator drain and keeps configured drain visible after clearing it', async () => {
		await page.viewport(320, 844);
		const volumeId = 'a'.repeat(64);
		const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
		let draining = false;
		const storageVolumes = vi
			.fn<VolumeController['storageVolumes']>()
			.mockImplementation(async (request) => {
				if (request.action.case === 'moves')
					return create(StorageVolumeResultSchema, { result: { case: 'jobs', value: {} } });
				if (request.action.case === 'setDraining') draining = request.action.value.draining;
				return create(StorageVolumeResultSchema, {
					result: {
						case: 'volumes',
						value: {
							configurationRevision: 'revision-one',
							runtimeAvailable: true,
							volumes: [
								{
									volumeId,
									online: true,
									configuredDraining: true,
									operatorDraining: draining
								}
							]
						}
					}
				});
			});
		try {
			const { container } = await render(NamedVolumesSection, {
				config: fixture(),
				controller: { storageVolumes, updateRuntimeConfiguration: vi.fn() },
				onsaved: vi.fn()
			});
			await page.getByRole('button', { name: `Stop new writes to ${volumeId}` }).click();
			expect(
				storageVolumes.mock.calls.filter(([request]) => request.action.case === 'setDraining')
			).toHaveLength(0);
			expect(container.scrollWidth).toBeLessThanOrEqual(320);
			confirm.mockReturnValue(true);
			await page.getByRole('button', { name: `Stop new writes to ${volumeId}` }).click();
			await expect.element(page.getByText('Operator drain is active.')).toBeVisible();
			const mutation = storageVolumes.mock.calls.find(
				([request]) => request.action.case === 'setDraining'
			)?.[0];
			expect(mutation?.action).toMatchObject({
				case: 'setDraining',
				value: {
					volumeId,
					draining: true,
					expectedConfigurationRevision: 'revision-one'
				}
			});
			await page.getByRole('button', { name: `Clear operator drain for ${volumeId}` }).click();
			await expect.element(page.getByText('Operator drain is active.')).not.toBeInTheDocument();
			await expect.element(page.getByText('Draining from saved configuration.')).toBeVisible();
		} finally {
			confirm.mockRestore();
		}
	});
	it('confirms persisted removal after renaming and preserves cancelled edits', async () => {
		const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
		try {
			await render(VolumeDraftEditor, {
				config: fixture(),
				controller: { storageVolumes: vi.fn(), updateRuntimeConfiguration: vi.fn() },
				onsaved: vi.fn(),
				oncancel: vi.fn()
			});
			await page.getByLabelText('ID', { exact: true }).fill('renamed');
			await page.getByRole('button', { name: 'Remove volume 1' }).click();
			expect(confirm).toHaveBeenCalledWith(expect.stringContaining('archive'));
			await expect.element(page.getByLabelText('ID', { exact: true })).toHaveValue('renamed');
			confirm.mockReturnValue(true);
			await page.getByRole('button', { name: 'Remove volume 1' }).click();
			await expect.element(page.getByLabelText('ID', { exact: true })).not.toBeInTheDocument();
			await page.getByRole('button', { name: 'Add volume', exact: true }).click();
			await page.getByRole('button', { name: 'Remove volume 1' }).click();
			expect(confirm).toHaveBeenCalledTimes(2);
		} finally {
			confirm.mockRestore();
		}
	});
	it('keeps exact dirty inputs and secret references across status refresh on a narrow screen', async () => {
		await page.viewport(390, 844);
		const config = fixture();
		const update = vi
			.fn<VolumeController['updateRuntimeConfiguration']>()
			.mockResolvedValue({ config, restart_required: false });
		const controller: VolumeController = {
			updateRuntimeConfiguration: update,
			storageVolumes: vi.fn().mockResolvedValue(
				create(StorageVolumeResultSchema, {
					result: {
						case: 'volumes',
						value: {
							runtimeAvailable: false,
							volumes: [{ volumeId: 'archive', online: false, ownedBytes: 9007199254740993n }]
						}
					}
				})
			)
		};
		const { container } = await render(NamedVolumesSection, {
			config,
			controller,
			onsaved: vi.fn()
		});
		await page.getByRole('button', { name: 'Edit volume draft' }).click();
		const capacity = page.getByLabelText('Capacity in bytes (blank means unlimited)');
		await expect.element(capacity).toHaveValue('9007199254740993');
		await capacity.fill('9007199254740995');
		await page
			.getByRole('combobox', { name: /^State/ })
			.selectOptions(page.getByRole('option', { name: 'Enabled', exact: true }));
		await page.getByRole('button', { name: 'Refresh volume status' }).click();
		await expect.element(capacity).toHaveValue('9007199254740995');
		await expect
			.element(page.getByLabelText('Root or secret reference'))
			.toHaveValue('{secret:ROOT}');
		expect(container.scrollWidth).toBeLessThanOrEqual(390);
		await page.getByRole('button', { name: 'Save volume draft' }).click();
		await expect.poll(() => update.mock.calls.length).toBe(1);
		const saved = update.mock.calls[0][0];
		expect(saved.expected_configuration_revision).toBe('revision-one');
		expect(saved.storage.named_volumes?.volumes[0].capacityBytes).toBe(9007199254740995n);
		expect(saved.storage.named_volumes?.volumes[0].root).toBe('{secret:ROOT}');
		expect(saved.storage.named_volumes?.volumes[0].state).toBe(StorageVolumeState.ENABLED);
		expect(saved.storage.medium_term_path).toBe('/legacy');
	});
});
