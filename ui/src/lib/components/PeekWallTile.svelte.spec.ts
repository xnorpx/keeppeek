import { tick } from 'svelte';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { CameraListItem } from '$lib/types';
import '../../app.css';

vi.mock('$lib/stream-peer-context', () => ({
	useLivePeer: () => ({
		track: () => ({ status: 'queued', subscribed: false }),
		attach: () => () => {},
		markPlaying: () => {},
		sessionId: null,
		estimatedBitrateBps: null,
		connectionState: 'new',
		iceConnectionState: 'new'
	})
}));

import PeekCameraTile from './PeekCameraTile.svelte';

const camera: CameraListItem = {
	id: 'front',
	name: 'Front camera with a long location name',
	ip: '192.0.2.1',
	manufacturer: null,
	model: null,
	firmware_version: null,
	is_reolink: false,
	capabilities: {
		ptz: false,
		audio: false,
		events: false,
		recording: true,
		analytics: false,
		imaging: false,
		two_way_audio: false
	},
	profiles: []
};

describe('Peek wall tile presentation', () => {
	it.each(['16:9', '4:3', 'native'] as const)(
		'keeps %s geometry independent from media fit',
		async (tileShape) => {
			const { container, rerender } = await render(PeekCameraTile, {
				camera,
				stream: 'sub',
				tileShape,
				mediaFit: 'contain',
				onfocus: () => {}
			});
			const video = container.querySelector<HTMLVideoElement>('video')!;
			const tile = container.querySelector<HTMLElement>('[data-peek-camera]')!;
			expect(tile.dataset.peekTileShape).toBe(tileShape);
			expect(getComputedStyle(video).objectFit).toBe('contain');
			await rerender({ mediaFit: 'cover' });
			expect(container.querySelector('video')).toBe(video);
			expect(getComputedStyle(video).objectFit).toBe('cover');
			expect(container.textContent).toContain('Cropped');
		}
	);

	it.each([
		[1_080, 1_920],
		[3_840, 480]
	])('latches native %ix%i metadata through quality changes', async (width, height) => {
		const { container, rerender } = await render(PeekCameraTile, {
			camera,
			stream: 'sub',
			tileShape: 'native',
			onfocus: () => {}
		});
		const video = container.querySelector<HTMLVideoElement>('video')!;
		const tile = container.querySelector<HTMLElement>('[data-peek-camera]')!;
		expect(tile.style.getPropertyValue('--peek-tile-ratio')).toBe(String(16 / 9));
		Object.defineProperties(video, {
			videoWidth: { configurable: true, value: width },
			videoHeight: { configurable: true, value: height }
		});
		video.dispatchEvent(new Event('loadedmetadata'));
		await tick();
		expect(tile.style.getPropertyValue('--peek-tile-ratio')).toBe(String(width / height));
		await rerender({ stream: 'main' });
		Object.defineProperties(video, {
			videoWidth: { configurable: true, value: 1_920 },
			videoHeight: { configurable: true, value: 1_080 }
		});
		video.dispatchEvent(new Event('resize'));
		await tick();
		expect(tile.style.getPropertyValue('--peek-tile-ratio')).toBe(String(width / height));
	});

	it('shows admission and frame evidence instead of a connection spinner', async () => {
		const { container } = await render(PeekCameraTile, {
			camera,
			stream: 'sub',
			admission: 'capacity',
			onfocus: () => {}
		});
		expect(container.textContent).toContain('Device stream budget reached');
		expect(container.textContent).toContain('No frame received');
		expect(container.textContent).not.toContain('CONNECTING');
		expect(container.textContent).not.toContain('Queued');
	});
});
