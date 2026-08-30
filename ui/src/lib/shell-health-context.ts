import { getContext, setContext } from 'svelte';
import type { ServerHealthResponse } from './types';

const SHELL_HEALTH_PUBLISHER_CONTEXT = Symbol('keeppeek.shell-health-publisher');

export type ShellHealthPublisher = (health: ServerHealthResponse | null) => void;

export function setShellHealthPublisher(publisher: ShellHealthPublisher): ShellHealthPublisher {
	return setContext(SHELL_HEALTH_PUBLISHER_CONTEXT, publisher);
}

export function useShellHealthPublisher(): ShellHealthPublisher {
	const publisher = getContext<ShellHealthPublisher | undefined>(SHELL_HEALTH_PUBLISHER_CONTEXT);
	if (!publisher) throw new Error('Shell health publisher context is unavailable');
	return publisher;
}
