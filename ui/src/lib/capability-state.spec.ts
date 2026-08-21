import { describe, expect, it } from 'vitest';
import { CapabilityState } from './capability-state.svelte';

describe('capability command state', () => {
	it('accepts only exact known capability identifiers', () => {
		const state = new CapabilityState([
			'keeppeek.media-export.v1',
			'keeppeek.media-export.v2',
			'unknown.v1'
		]);

		expect([...state.advertised]).toEqual(['keeppeek.media-export.v1']);
		expect(state.supports('keeppeek.media-export.v1')).toBe(true);
		expect(state.supports('keeppeek.identity.v1')).toBe(false);
	});

	it('blocks a draft before it begins when support is missing', () => {
		const state = new CapabilityState();

		expect(state.begin('export', 'keeppeek.media-export.v1')).toBe(false);
		expect(state.command('export')).toEqual({
			commandId: 'export',
			capability: 'keeppeek.media-export.v1',
			phase: 'blocked',
			error: null,
			capabilityLost: false
		});
		expect(state.label('keeppeek.media-export.v1')).toBe(
			'Server update required · keeppeek.media-export.v1'
		);
	});

	it('freezes submission when capability support disappears mid-draft', () => {
		const state = new CapabilityState(['keeppeek.runtime-config.v1']);

		expect(state.begin('save-storage', 'keeppeek.runtime-config.v1')).toBe(true);
		state.updateAdvertised([]);

		expect(state.command('save-storage')?.phase).toBe('blocked');
		expect(state.command('save-storage')?.capabilityLost).toBe(true);
		expect(state.submit('save-storage')).toBe(false);
	});

	it('preserves the exact failure until retry or reset', () => {
		const state = new CapabilityState(['keeppeek.runtime-config.v1']);
		state.begin('save-storage', 'keeppeek.runtime-config.v1');
		expect(state.submit('save-storage')).toBe(true);
		state.fail('save-storage', 'Revision 42 replaced this draft');

		expect(state.command('save-storage')?.phase).toBe('failed');
		expect(state.command('save-storage')?.error).toBe('Revision 42 replaced this draft');
		expect(state.submit('save-storage')).toBe(true);
		expect(state.command('save-storage')?.error).toBeNull();
		state.succeed('save-storage');
		expect(state.command('save-storage')?.phase).toBe('succeeded');
		state.reset('save-storage');
		expect(state.command('save-storage')?.phase).toBe('idle');
	});
});
