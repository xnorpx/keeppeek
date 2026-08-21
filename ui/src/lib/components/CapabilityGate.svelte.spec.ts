import { page } from 'vitest/browser';
import { describe, expect, it } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { CapabilityState } from '$lib/capability-state.svelte';
import CapabilityGate from './CapabilityGate.svelte';

describe('CapabilityGate', () => {
	it('names the unavailable action and exact required capability', async () => {
		await render(CapabilityGate, {
			props: {
				action: 'Export clip',
				capability: 'keeppeek.media-export.v1',
				state: new CapabilityState()
			}
		});

		const gate = page.getByRole('status');
		await expect
			.element(gate)
			.toHaveTextContent('Export clip Server update required · keeppeek.media-export.v1');
		await expect.element(gate).toHaveAttribute('data-capability', 'keeppeek.media-export.v1');
	});

	it('reactively removes the gate when exact support appears', async () => {
		const state = new CapabilityState();
		await render(CapabilityGate, {
			props: {
				action: 'Add a rule',
				capability: 'keeppeek.rules.v1',
				state
			}
		});

		await expect.element(page.getByRole('status')).toBeInTheDocument();
		state.updateAdvertised(['keeppeek.rules.v1']);
		await expect.element(page.getByRole('status')).not.toBeInTheDocument();
	});
});
