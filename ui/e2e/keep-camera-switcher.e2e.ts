import { expect, test } from '@playwright/test';
import type { CameraListItem } from '../src/lib/types';
import { mockControlPeer } from './fixtures/control-peer';
import { keepModeDate, mockKeepModes } from './fixtures/keep-modes';

const day = '2026-08-10';
const dayStartMs = Date.UTC(2026, 7, 10);

function camera(id: string, name: string): CameraListItem {
	return {
		id,
		ip: `192.0.2.${id.length + 10}`,
		name,
		manufacturer: 'Test',
		model: 'Fixture',
		firmware_version: null,
		is_reolink: false,
		profiles: [
			{ name: 'Main', stream: 'main', encoding: 'h265', resolution: '1920x1080', framerate: 20 },
			{ name: 'Sub', stream: 'sub', encoding: 'h264', resolution: '640x360', framerate: 15 }
		]
	};
}

test('switches Keep cameras without rendering secondary camera previews', async ({ page }) => {
	const cameras = [
		camera('deck', 'Deck'),
		camera('garden', 'Garden'),
		camera('gate', 'Gate'),
		camera('yard', 'Yard'),
		camera('shed', 'Shed')
	];
	await page.addInitScript(() => {
		Object.defineProperty(HTMLMediaElement.prototype, 'play', {
			configurable: true,
			value() {
				this.dataset.playRequested = 'true';
				return Promise.resolve();
			}
		});
	});
	const requests = await mockControlPeer(page, {
		cameras,
		storedRanges: cameras.flatMap((camera) =>
			(['main', 'sub'] as const).map((streamId) => ({
				sourceId: camera.id,
				streamId,
				startMs: dayStartMs + 10 * 60 * 60_000,
				endMs: dayStartMs + 11 * 60 * 60_000
			}))
		)
	});
	await page.goto(`/keep?camera=deck&stream=main&date=${day}`);

	const player = page.getByRole('region', { name: 'Recorded video player' });
	const cameraSwitcher = page.locator('[data-camera-switcher]');
	await expect(cameraSwitcher).toHaveAttribute('data-selected-camera', 'deck');
	await expect(page.getByRole('button', { name: 'Previous camera, Shed' })).toBeVisible();
	await expect(page.getByRole('button', { name: 'Next camera, Garden' })).toBeVisible();
	await expect(page.getByRole('region', { name: 'Other camera recordings' })).toHaveCount(0);
	await expect(page.locator('video[data-recording-preview]')).toHaveCount(0);
	expect(requests.storedOpens.filter((request) => request.sourceId !== 'deck')).toHaveLength(0);

	await page.getByRole('button', { name: 'Choose camera, Deck, 1 of 5' }).click();
	const cameraDialog = page.getByRole('dialog', { name: 'Choose a Keep camera' });
	const cameraSearch = page.getByRole('searchbox', { name: 'Find a Keep camera' });
	await expect(cameraDialog).toBeVisible();
	await expect(cameraSearch).toBeFocused();
	await cameraSearch.fill('Yard');
	await cameraDialog.getByRole('option', { name: /Yard/ }).click();

	await expect(page).toHaveURL(new RegExp(`/keep\\?camera=yard&stream=sub&date=${day}`));
	await expect(player).toHaveAttribute('data-camera-transition', 'loading');
	const yardVideo = player.locator('video:not([data-recording-preview])');
	await yardVideo.dispatchEvent('loadeddata');
	await expect(player).toHaveAttribute('data-camera-transition-direction', 'next');
	await expect(yardVideo).toHaveClass(/camera-switch-enter-next/);
	await expect(player).toHaveAttribute('data-camera-transition', 'idle');
	expect(requests.storedOpens.filter((request) => request.sourceId === 'yard')).toHaveLength(1);
	await expect(page.getByRole('region', { name: 'Other camera recordings' })).toHaveCount(0);
	await expect(page.locator('video[data-recording-preview]')).toHaveCount(0);

	await page.getByRole('button', { name: 'Previous camera, Gate' }).click();
	await expect(page).toHaveURL(new RegExp(`/keep\\?camera=gate&stream=sub&date=${day}`));
	await expect(player).toHaveAttribute('data-camera-transition-direction', 'previous');
	const gateVideo = player.locator('video:not([data-recording-preview])');
	await gateVideo.dispatchEvent('loadeddata');
	await expect(gateVideo).toHaveClass(/camera-switch-enter-previous/);
	await expect(page.getByRole('region', { name: 'Other camera recordings' })).toHaveCount(0);
	await expect(page.locator('video[data-recording-preview]')).toHaveCount(0);
	expect(requests.storedOpens.filter((request) => request.sourceId === 'gate')).toHaveLength(1);
});

test('switching cameras clears the previous export range while the new recording query is pending', async ({
	page
}) => {
	const start = dayStartMs + 10 * 60 * 60_000;
	const requested = Promise.withResolvers<void>();
	const delayed = Promise.withResolvers<void>();
	await mockControlPeer(page, {
		cameras: [camera('deck', 'Deck'), camera('garden', 'Garden')],
		capabilityIds: ['keeppeek.guest-range.v1'],
		storedRanges: [
			{ sourceId: 'deck', streamId: 'sub', startMs: start, endMs: start + 60_000 },
			{ sourceId: 'garden', streamId: 'sub', startMs: start - 2_000, endMs: start + 40_000 }
		],
		beforeStoredTimelineQuery(sourceIds) {
			if (sourceIds.includes('garden')) {
				requested.resolve();
				return delayed.promise;
			}
		}
	});
	await page.goto(`/keep?camera=deck&stream=sub&at=${start + 5_000}&mode=export`);
	const panel = page.locator('[data-keep-export]');
	const share = page.getByRole('button', { name: 'Share recording range' });
	await expect(panel).toHaveAttribute('data-export-start-ms', String(start), { timeout: 45_000 });
	await expect(share).toBeVisible();
	await page.getByRole('button', { name: 'Next camera, Garden' }).click();
	try {
		await requested.promise;
		await expect(share).toHaveCount(0);
		await expect(panel).toHaveCount(0);
	} finally {
		delayed.resolve();
	}
	await expect(panel).toHaveAttribute('data-export-start-ms', String(start - 2_000));
	await expect(panel).toHaveAttribute('data-export-end-ms', String(start + 40_000));
	await expect(share).toBeVisible();
});

test('switching cameras closes the previous camera event drawer immediately', async ({ page }) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	await mockKeepModes(page, 2, {
		capabilityIds: ['keeppeek.event-workflow.v1', 'keeppeek.guest-range.v1']
	});
	await page.goto(`/keep?camera=front-door&stream=main&date=${keepModeDate}&mode=stories`);
	await page.getByRole('button', { name: /Review story at/ }).click();
	const detail = page.getByRole('complementary', { name: 'Event detail' });
	await expect(detail).toBeVisible();
	const next = page.getByRole('button', { name: 'Next camera, Camera 2' });
	await next.focus();
	await next.press('Enter');
	await expect(page).toHaveURL(/camera=camera-2/);
	await expect(detail).toHaveCount(0);
});
