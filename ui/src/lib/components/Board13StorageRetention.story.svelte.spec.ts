import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Board13StorageRetentionStory from '../../../visual-harness/stories/Board13StorageRetentionStory.svelte';
import StorageSettingsEditor from './StorageSettingsEditor.svelte';
import type { SanitizedConfig, ServerHealthResponse } from '$lib/types';

function roundedSize(element: Element): [number, number] {
	const bounds = element.getBoundingClientRect();
	return [Math.round(bounds.width), Math.round(bounds.height)];
}

describe('Board 13 Storage and Retention story', () => {
	it('renders the production owner in the exact Paper shell and five content bands', async () => {
		await page.viewport(1440, 1250);
		const { container } = await render(Board13StorageRetentionStory);
		await document.fonts.ready;
		const frame = container.querySelector<HTMLElement>(
			'[data-paper-scenario="settings.desktop.storage-retention"]'
		);
		expect(frame).not.toBeNull();
		expect(roundedSize(frame!)).toEqual([1440, 1163]);

		const owner = frame!.querySelector<HTMLElement>('[data-storage-retention-paper-frame]');
		expect(owner).not.toBeNull();
		expect([...owner!.children].map(roundedSize)).toEqual([
			[64, 1161],
			[1374, 1161]
		]);
		const main = owner!.children[1];
		expect([...main.children].map(roundedSize)).toEqual([
			[1374, 52],
			[1374, 1109]
		]);
		expect([...main.children[1].children].map(roundedSize)).toEqual([
			[240, 1109],
			[1134, 1109]
		]);
		const bands = [...owner!.querySelectorAll<HTMLElement>('[data-storage-band]')];
		expect(bands.map(roundedSize)).toEqual([
			[1070, 84],
			[1070, 128],
			[1070, 235],
			[1070, 216],
			[1070, 278]
		]);
		expect([...bands[2].children].map(roundedSize)).toEqual([
			[344, 235],
			[343, 235],
			[343, 235]
		]);
	});

	it('separates measured capacity and fixed pruning from unavailable history and policy', async () => {
		await page.viewport(1440, 1250);
		const { container } = await render(Board13StorageRetentionStory);
		const frame = container.querySelector<HTMLElement>('[data-paper-scenario]');
		expect(frame).not.toBeNull();

		await expect.element(page.getByText('11 days', { exact: true })).toBeVisible();
		await expect.element(page.getByText('OLDEST FOOTAGE ON DISK', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Prune the oldest dated recordings', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByText('Stop recording when full', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Camera retention evidence', { exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByText(/Server update required · keeppeek\.offsite-archive\.v1/).first())
			.toBeVisible();
		expect(frame!.textContent).toContain('Catalog bounds observe 11 days');
		expect(frame!.textContent).not.toContain('smb://nas.lan');
		expect(frame!.textContent).not.toContain('Front Door');
		expect(frame!.textContent).not.toContain('3 OF 42');
	});

	it('reviews one storage location and an explicit restart migration before staging', async () => {
		await page.viewport(1200, 950);
		const onsave = vi.fn();
		const config = {
			host: '0.0.0.0',
			port: 3000,
			camera_count: 9,
			storage: {
				medium_term_path: '/mnt/keeppeek',
				long_term_path: '/mnt/keeppeek',
				recording_catalog_path: '/mnt/keeppeek/recordings.db',
				event_thumbnail_path: '/mnt/keeppeek/.event-thumbnails',
				event_thumbnail_max_mb: 1024,
				short_term_secs: 90,
				medium_term_secs: 1200,
				flush_interval_secs: 60,
				write_buffer_bytes: 8_192,
				long_term_max_gb: 2500
			},
			recording_estimate: {
				estimated_bitrate_bps: 8_576_000,
				bytes_per_day: 92_620_800_000,
				known_streams: 9,
				unknown_streams: 0,
				estimated_retention_days: 29
			}
		} satisfies SanitizedConfig;
		const health = {
			system: {
				disks: [
					{
						name: 'WD Red 8 TB',
						kind: 'SSD',
						file_system: 'ext4',
						mount_point: '/mnt/keeppeek',
						total_bytes: 8_000_000_000_000,
						available_bytes: 2_320_000_000_000,
						used_bytes: 5_680_000_000_000,
						removable: false,
						stores_recordings: true
					},
					{
						name: 'KeepPeek Archive',
						kind: 'SSD',
						file_system: 'apfs',
						mount_point: '/Volumes/KeepPeek Archive',
						total_bytes: 4_000_000_000_000,
						available_bytes: 3_200_000_000_000,
						used_bytes: 800_000_000_000,
						removable: true,
						stores_recordings: false
					}
				]
			},
			storage: {
				long_term_max_bytes: 2_684_354_560_000,
				catalog: { fragment_bytes: 1_800_000_000_000 }
			}
		} as ServerHealthResponse;

		await render(StorageSettingsEditor, {
			config,
			health,
			oncancel: vi.fn(),
			onsave
		});
		await page.getByLabelText('Folder path').fill('/Volumes/KeepPeek Archive/recordings');
		await page.getByLabelText('Maximum recording storage (GiB)').fill('2048');
		await page.getByLabelText('Move existing storage during restart').click();

		await expect
			.element(page.getByText('KeepPeek Archive · 2.9 TiB free', { exact: true }).first())
			.toHaveTextContent('KeepPeek Archive · 2.9 TiB free');
		await expect.element(page.getByText('Existing files', { exact: true })).toBeVisible();
		await expect
			.element(page.getByText('Advanced storage paths and writer controls'))
			.toBeVisible();
		await expect.element(page.getByLabelText('Host')).not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Continue to review' }).click();
		await expect
			.element(page.getByRole('heading', { name: 'Review storage changes' }))
			.toHaveFocus();
		await expect.element(page.getByText('Move during restart', { exact: true })).toBeVisible();
		await page.getByRole('button', { name: 'Stage storage changes' }).click();

		expect(onsave).toHaveBeenCalledWith(
			expect.objectContaining({
				host: '0.0.0.0',
				port: 3000,
				move_existing_recordings: true,
				storage: expect.objectContaining({
					long_term_path: '/Volumes/KeepPeek Archive/recordings',
					long_term_max_gb: 2048
				})
			})
		);
	});
});
