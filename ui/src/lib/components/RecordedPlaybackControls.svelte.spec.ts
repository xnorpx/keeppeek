import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import RecordedPlaybackControls from './RecordedPlaybackControls.svelte';

function props() {
	return {
		playing: false,
		muted: true,
		volume: 1,
		rate: 1,
		positionSeconds: 10,
		durationSeconds: 120,
		disabled: false,
		fullscreenTarget: null,
		ontoggleplay: vi.fn(),
		ontogglemute: vi.fn(),
		onseek: vi.fn(),
		onskip: vi.fn(),
		onvolumechange: vi.fn(),
		onratechange: vi.fn()
	};
}

describe('RecordedPlaybackControls', () => {
	it('exposes independent, named transport commands and current playback time', async () => {
		const actions = props();
		const view = await render(RecordedPlaybackControls, { props: actions });
		await userEvent.click(page.getByRole('button', { name: 'Play recording', exact: true }));
		await userEvent.click(page.getByRole('button', { name: 'Unmute recording', exact: true }));
		await userEvent.click(page.getByRole('button', { name: 'Back 10 seconds', exact: true }));
		await userEvent.click(page.getByRole('button', { name: 'Forward 10 seconds', exact: true }));
		expect(actions.ontoggleplay).toHaveBeenCalledOnce();
		expect(actions.ontogglemute).toHaveBeenCalledOnce();
		expect(actions.onskip).toHaveBeenCalledWith(-10);
		expect(actions.onskip).toHaveBeenCalledWith(10);
		await expect.element(page.getByLabelText('Playback time')).toHaveTextContent('0:10 / 2:00');
		await view.rerender({ playing: true, muted: false });
		await expect
			.element(page.getByRole('button', { name: 'Pause recording', exact: true }))
			.toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Mute recording', exact: true }))
			.toBeVisible();
	});

	it('supports keyboard position and volume sliders and an explicit playback speed', async () => {
		const actions = props();
		await render(RecordedPlaybackControls, { props: actions });
		const position = page.getByRole('slider', { name: 'Recording position', exact: true });
		(position.element() as HTMLInputElement).focus();
		await userEvent.keyboard('{Home}{ArrowRight}');
		expect(actions.onseek).toHaveBeenLastCalledWith(1);
		const volume = page.getByRole('slider', { name: 'Recording volume', exact: true });
		(volume.element() as HTMLInputElement).focus();
		await userEvent.keyboard('{Home}{ArrowRight}');
		expect(actions.onvolumechange).toHaveBeenLastCalledWith(0.05);
		await userEvent.selectOptions(
			page.getByRole('combobox', { name: 'Playback speed', exact: true }),
			'2'
		);
		expect(actions.onratechange).toHaveBeenLastCalledWith(2);
	});

	it('disables unavailable media and never uses a non-finite seek range', async () => {
		await render(RecordedPlaybackControls, {
			props: { ...props(), disabled: true, durationSeconds: Number.POSITIVE_INFINITY }
		});
		await expect
			.element(page.getByRole('button', { name: 'Play recording', exact: true }))
			.toBeDisabled();
		await expect
			.element(page.getByRole('slider', { name: 'Recording position', exact: true }))
			.toBeDisabled();
		await expect
			.element(page.getByRole('button', { name: 'Enter recording fullscreen', exact: true }))
			.toBeDisabled();
	});
});
