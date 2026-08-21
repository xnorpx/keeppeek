import { getContext, setContext } from 'svelte';
import { AppearanceState } from '$lib/appearance-state.svelte';

const APPEARANCE_CONTEXT = Symbol('keeppeek.appearance');

export function setAppearanceState(): AppearanceState {
	return setContext(APPEARANCE_CONTEXT, new AppearanceState());
}

export function useAppearanceState(): AppearanceState {
	const state = getContext<AppearanceState | undefined>(APPEARANCE_CONTEXT);
	if (state === undefined) throw new Error('Appearance context is unavailable');
	return state;
}
