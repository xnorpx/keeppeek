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
});
