export type KeyboardOverlayMode = 'commands' | 'help' | null;

export type GlobalKeyboardAction =
	| { type: 'cancel-navigation' }
	| { type: 'close-overlay' }
	| { type: 'focus-search' }
	| { type: 'navigate'; href: string }
	| { type: 'open-commands' }
	| { type: 'open-help' }
	| { type: 'start-navigation' };

export type GlobalKeyboardState = {
	overlay: KeyboardOverlayMode;
	navigationPending: boolean;
	typing: boolean;
};

export type ShortcutKey = {
	key: string;
	altKey?: boolean;
	ctrlKey?: boolean;
	metaKey?: boolean;
	repeat?: boolean;
	shiftKey?: boolean;
};

export const keyboardDestinations = [
	{ key: 'p', label: 'Peek', href: '/' },
	{ key: 'k', label: 'Keep', href: '/keep' },
	{ key: 'e', label: 'Events', href: '/events' },
	{ key: 'c', label: 'Cameras', href: '/cameras' },
	{ key: 'h', label: 'Health', href: '/system-health' },
	{ key: 's', label: 'Settings', href: '/settings' }
] as const;

export function resolveGlobalKeyboardAction(
	event: ShortcutKey,
	state: GlobalKeyboardState
): GlobalKeyboardAction | null {
	const key = event.key.toLowerCase();
	const commandModifier = Boolean(event.metaKey || event.ctrlKey);
	const otherModifier = Boolean(event.altKey);

	if (state.overlay !== null) {
		return event.key === 'Escape' ? { type: 'close-overlay' } : null;
	}
	if (commandModifier && !otherModifier && key === 'k' && !event.repeat) {
		return { type: 'open-commands' };
	}
	if (state.typing) {
		return state.navigationPending ? { type: 'cancel-navigation' } : null;
	}
	if (state.navigationPending) {
		if (commandModifier || otherModifier) return { type: 'cancel-navigation' };
		const destination = keyboardDestinations.find((item) => item.key === key);
		return destination
			? { type: 'navigate', href: destination.href }
			: { type: 'cancel-navigation' };
	}
	if (commandModifier || otherModifier || event.repeat) return null;
	if (key === 'g') return { type: 'start-navigation' };
	if (event.key === '?' || (event.key === '/' && event.shiftKey)) return { type: 'open-help' };
	if (event.key === '/') return { type: 'focus-search' };
	return null;
}

export function isKeyboardTypingTarget(target: EventTarget | null): boolean {
	if (!(target instanceof HTMLElement)) return false;
	return (
		target instanceof HTMLInputElement ||
		target instanceof HTMLTextAreaElement ||
		target instanceof HTMLSelectElement ||
		target.isContentEditable ||
		target.closest('[contenteditable="true"], [role="textbox"]') !== null
	);
}
