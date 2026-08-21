import { getContext, setContext } from 'svelte';
import { ControlClient } from './control-client';

const CONTROL_CLIENT_CONTEXT = Symbol('keeppeek.control-client');

export function setControlClient(client = new ControlClient()): ControlClient {
	return setContext(CONTROL_CLIENT_CONTEXT, client);
}

export function useControlClient(): ControlClient {
	const client = getContext<ControlClient | undefined>(CONTROL_CLIENT_CONTEXT);
	if (!client) throw new Error('Control client context is unavailable');
	return client;
}
