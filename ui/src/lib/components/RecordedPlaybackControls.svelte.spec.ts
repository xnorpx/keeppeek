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
	it('reads the selected timestamp only on request and preserves subsecond positioning', async () => {
		const view = await render(RecordedPlaybackControls, {
			props: {
				...props(),
				positionSeconds: 1.125,
				frameTimeNs: 1_125_000_000n,
				frameSourceTimeNs: 1_100_000_000n
			}
		});
		const announced = page.getByRole('status', { name: 'Requested frame time' });
		await expect.element(announced).toBeEmptyDOMElement();
		expect(
			(page.getByRole('slider', { name: 'Recording position' }).element() as HTMLInputElement)
				.valueAsNumber
		).toBe(1.125);
		await userEvent.click(page.getByRole('button', { name: 'Read selected frame time' }));
		await expect
			.element(announced)
			.toHaveTextContent('Recorded frame 1970-01-01T00:00:01.125000000Z');
		await view.rerender({ frameTimeNs: 1_250_000_000n, frameLoading: true });
		await expect.element(announced).toHaveTextContent('01.125000000Z');
		await expect.element(page.getByText('Loading recorded picture…')).toBeVisible();
		await userEvent.click(page.getByRole('button', { name: 'Read selected frame time' }));
		await expect.element(announced).toHaveTextContent('01.250000000Z');
	});

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

	it('keeps the compact primary row intact and exposes touch-sized step controls while paused', async () => {
		const onframe = vi.fn();
		const view = await render(RecordedPlaybackControls, {
			props: { ...props(), onframe, onoptions: vi.fn(), frameTimeNs: 1_234_567_891n }
		});
		const root = view.container.querySelector('[data-recorded-playback-controls]') as HTMLElement;
		root.style.width = '390px';
		const play = page.getByRole('button', { name: 'Play recording', exact: true }).element();
		const mute = page.getByRole('button', { name: 'Unmute recording', exact: true }).element();
		const back = page.getByRole('button', { name: 'Previous recorded frame' });
		const next = page.getByRole('button', { name: 'Next recorded frame' });
		expect(play.getBoundingClientRect().top).toBe(mute.getBoundingClientRect().top);
		expect(back.element().getBoundingClientRect().top).toBeGreaterThan(
			play.getBoundingClientRect().bottom
		);
		expect(next.element().getBoundingClientRect().width).toBeGreaterThanOrEqual(44);
		expect(next.element().getBoundingClientRect().height).toBeGreaterThanOrEqual(44);
		await userEvent.click(back);
		await userEvent.click(next);
		expect(onframe.mock.calls).toEqual([[-1], [1]]);
		expect(root.querySelector('time')?.dateTime).toBe('1970-01-01T00:00:01.234567891Z');
		await view.rerender({ playing: true });
		await expect.element(next).not.toBeInTheDocument();
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
