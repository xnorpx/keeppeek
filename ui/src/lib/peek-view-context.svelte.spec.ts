import { describe, expect, it } from 'vitest';
import type { ControlClient } from './control-client';
import type { CameraListItem } from './types';
import { PeekViewState } from './peek-view-context.svelte';

describe('Peek view state', () => {
	it('rejects stale writes after access state resets', () => {
		const state = new PeekViewState();
		const previousGeneration = state.generation;
		expect(state.updateLayoutError(previousGeneration, 'Before reset')).toBe(true);

		state.reset();

		expect(state.updateLayoutError(previousGeneration, 'Leaked dashboard')).toBe(false);
		expect(state.layoutError).toBeNull();
		expect(state.updateLayoutError(state.generation, 'Current dashboard')).toBe(true);
		expect(state.layoutError).toBe('Current dashboard');
	});

	it('keeps a confirmed inventory ahead of a pending wall refresh and discards it on access reset', async () => {
		const state = new PeekViewState();
		const initial = Promise.withResolvers<CameraListItem[]>();
		const expanded = Promise.withResolvers<CameraListItem[]>();
		const revoked = Promise.withResolvers<CameraListItem[]>();
		const snapshots = [initial.promise, expanded.promise, revoked.promise];
		const controller = {
			getCameras: () => snapshots.shift(),
			getHealth: async () => null,
			getServerCapabilities: async () => ({ capabilityIds: [] })
		} as unknown as ControlClient;
		const old = state.refresh(controller);
		const confirmation = state.refreshCameraInventory(controller);
		const front = { id: 'front' } as CameraListItem;
		const porch = { id: 'porch' } as CameraListItem;
		expanded.resolve([front, porch]);
		await confirmation;
		initial.resolve([front]);
		await old;
		expect(state.cameras.map((camera) => camera.id)).toEqual(['front', 'porch']);
		const abandoned = state.refreshCameraInventory(controller);
		state.reset();
		revoked.resolve([porch]);
		await abandoned;
		expect(state.cameras).toEqual([]);
	});

	it('retains one transition frame until presentation or access reset', () => {
		const state = new PeekViewState();
		const dashboardFrame = {
			dataUrl: 'data:image/jpeg;base64,dashboard',
			destination: 'dashboard' as const,
			cameraId: null
		};
		const viewerFrame = {
			dataUrl: 'data:image/jpeg;base64,viewer',
			destination: 'viewer' as const,
			cameraId: 'front-door'
		};

		state.beginTransition(dashboardFrame);
		expect(state.transition).toEqual(dashboardFrame);
		state.beginTransition(viewerFrame);
		expect(state.transition).toEqual(viewerFrame);
		state.finishTransition(viewerFrame);
		expect(state.transition).toBeNull();

		state.beginTransition(dashboardFrame);
		state.reset();
		expect(state.transition).toBeNull();
	});

	it('merges per-camera transition frames and clears them on access reset', () => {
		const state = new PeekViewState();

		state.updateCameraFrames({ front: 'data:image/jpeg;base64,front' });
		state.updateCameraFrames({ yard: 'data:image/jpeg;base64,yard' });

		expect(state.cameraFrame('front')).toBe('data:image/jpeg;base64,front');
		expect(state.cameraFrame('yard')).toBe('data:image/jpeg;base64,yard');
		expect(state.cameraFrame('unknown')).toBeNull();

		state.reset();
		expect(state.cameraFrame('front')).toBeNull();
	});
});
