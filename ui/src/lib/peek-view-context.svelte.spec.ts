import { describe, expect, it } from 'vitest';
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
