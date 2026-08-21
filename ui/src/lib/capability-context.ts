import { getContext, setContext } from 'svelte';
import { CapabilityState } from './capability-state.svelte';

const CAPABILITY_CONTEXT = Symbol('keeppeek.capabilities');

export function setCapabilityState(advertisedCapabilities: Iterable<string> = []): CapabilityState {
	return setContext(CAPABILITY_CONTEXT, new CapabilityState(advertisedCapabilities));
}

export function useCapabilityState(): CapabilityState {
	const state = getContext<CapabilityState | undefined>(CAPABILITY_CONTEXT);
	if (state === undefined) throw new Error('Capability context is unavailable');
	return state;
}
