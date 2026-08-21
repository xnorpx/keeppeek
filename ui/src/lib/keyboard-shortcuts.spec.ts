import { describe, expect, it } from 'vitest';
import { resolveGlobalKeyboardAction, type GlobalKeyboardState } from './keyboard-shortcuts';

const idle: GlobalKeyboardState = { overlay: null, navigationPending: false, typing: false };

describe('global keyboard shortcuts', () => {
	it('opens discovery overlays with fixed global shortcuts', () => {
		expect(resolveGlobalKeyboardAction({ key: '?' }, idle)).toEqual({ type: 'open-help' });
		expect(resolveGlobalKeyboardAction({ key: '/', shiftKey: true }, idle)).toEqual({
			type: 'open-help'
		});
		expect(resolveGlobalKeyboardAction({ key: '/' }, idle)).toEqual({ type: 'focus-search' });
		expect(resolveGlobalKeyboardAction({ key: 'k', metaKey: true }, idle)).toEqual({
			type: 'open-commands'
		});
		expect(resolveGlobalKeyboardAction({ key: 'k', ctrlKey: true }, idle)).toEqual({
			type: 'open-commands'
		});
	});

	it('resolves the fixed two-key navigation destinations', () => {
		expect(resolveGlobalKeyboardAction({ key: 'g' }, idle)).toEqual({
			type: 'start-navigation'
		});
		expect(resolveGlobalKeyboardAction({ key: 'H' }, { ...idle, navigationPending: true })).toEqual(
			{ type: 'navigate', href: '/system-health' }
		);
	});

	it('lets typing beat every single-letter shortcut', () => {
		const typing = { ...idle, typing: true };
		expect(resolveGlobalKeyboardAction({ key: 'g' }, typing)).toBeNull();
		expect(resolveGlobalKeyboardAction({ key: '?' }, typing)).toBeNull();
		expect(resolveGlobalKeyboardAction({ key: 'k', metaKey: true }, typing)).toEqual({
			type: 'open-commands'
		});
	});

	it('closes only an active global overlay with Escape', () => {
		expect(resolveGlobalKeyboardAction({ key: 'Escape' }, idle)).toBeNull();
		expect(resolveGlobalKeyboardAction({ key: 'Escape' }, { ...idle, overlay: 'help' })).toEqual({
			type: 'close-overlay'
		});
	});

	it('cancels an incomplete chord instead of leaking a command', () => {
		expect(resolveGlobalKeyboardAction({ key: 'x' }, { ...idle, navigationPending: true })).toEqual(
			{ type: 'cancel-navigation' }
		);
	});
});
