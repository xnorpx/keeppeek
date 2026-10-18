import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { CameraSettings } from '$lib/types';
import '../../app.css';
import RecordedAudioSettings from './RecordedAudioSettings.svelte';

function camera(overrides: Partial<CameraSettings> = {}): CameraSettings {
	return {
		id: '192.0.2.10',
		ip: '192.0.2.10',
		configuration_revision: 'revision-one',
		display_name: 'Front Door',
		manufacturer_override: null,
		username_configured: true,
		password_configured: true,
		onvif_port: null,
		http_port: null,
		main_rtsp_url: null,
		sub_rtsp_url: null,
		uid_configured: false,
		backend: 'retina',
		transport: 'tcp',
		record_generic_motion_events: false,
		recording_mode: 'both',
		event_recording_duration_secs: 60,
		health: 'healthy',
		model: null,
		recorded_audio: { default_offset_ms: 250, main_offset_ms: 0 },
		...overrides
	};
}

describe('recorded-audio camera settings', () => {
	it('shows inheritance separately from explicit disabled zero and leaves live correction unavailable', async () => {
		await render(RecordedAudioSettings, { props: { camera: camera(), onsave: vi.fn() } });
		await expect
			.element(page.getByRole('combobox', { name: 'Camera default mode' }))
			.toHaveValue('custom');
		await expect
			.element(page.getByRole('combobox', { name: 'Main stream mode' }))
			.toHaveValue('custom');
		await expect
			.element(page.getByRole('combobox', { name: 'Sub stream mode' }))
			.toHaveValue('inherit');
		await expect
			.element(page.getByRole('textbox', { name: 'Main stream offset (ms)' }))
			.toHaveValue('0');
		await expect.element(page.getByText('Live audio correction is unavailable.')).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Save recorded timing' })).toBeDisabled();
	});

	it('saves only the full timing object and captured revision, preserving explicit zero', async () => {
		const onsave = vi.fn(async () =>
			camera({
				configuration_revision: 'revision-two',
				recorded_audio: { default_offset_ms: 400, main_offset_ms: 0 }
			})
		);
		await render(RecordedAudioSettings, { props: { camera: camera(), onsave } });
		await page.getByRole('textbox', { name: 'Camera default offset (ms)' }).fill('400');
		await page.getByRole('button', { name: 'Save recorded timing' }).click();
		expect(onsave).toHaveBeenCalledExactlyOnceWith(
			{ default_offset_ms: 400, main_offset_ms: 0 },
			'revision-one'
		);
		await expect
			.element(page.getByText('Recorded timing saved. Camera recording continues.'))
			.toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Save recorded timing' })).toBeDisabled();
	});

	it('rejects fractional and out-of-range values while warning about a valid large correction', async () => {
		const onsave = vi.fn(async () => camera({ recorded_audio: { default_offset_ms: 1250 } }));
		await render(RecordedAudioSettings, { props: { camera: camera(), onsave } });
		for (const value of ['1.5', '5001']) {
			await page.getByRole('textbox', { name: 'Camera default offset (ms)' }).fill(value);
			await page.getByRole('button', { name: 'Save recorded timing' }).click();
			await expect
				.element(page.getByRole('alert'))
				.toMatchTextContent('whole number from -5000 to +5000 ms');
		}
		expect(onsave).not.toHaveBeenCalled();
		await page.getByRole('textbox', { name: 'Camera default offset (ms)' }).fill('1250');
		await expect.element(page.getByText(/Large offset/)).toBeVisible();
		await page.getByRole('button', { name: 'Save recorded timing' }).click();
		expect(onsave).toHaveBeenCalledOnce();
	});

	it('keeps a failed dirty draft and its revision across refresh until explicit reload', async () => {
		const onsave = vi.fn(async () => {
			throw new Error('Configuration changed; reload saved values.');
		});
		const view = await render(RecordedAudioSettings, { props: { camera: camera(), onsave } });
		await page.getByRole('textbox', { name: 'Camera default offset (ms)' }).fill('-300');
		await page.getByRole('button', { name: 'Save recorded timing' }).click();
		await expect.element(page.getByRole('alert')).toMatchTextContent('Configuration changed');
		await view.rerender({
			camera: camera({
				configuration_revision: 'revision-two',
				recorded_audio: { default_offset_ms: 800 }
			})
		});
		await expect
			.element(page.getByRole('textbox', { name: 'Camera default offset (ms)' }))
			.toHaveValue('-300');
		await page.getByRole('button', { name: 'Save recorded timing' }).click();
		expect(onsave).toHaveBeenLastCalledWith(
			{ default_offset_ms: -300, main_offset_ms: 0 },
			'revision-one'
		);
		await page.getByRole('button', { name: 'Reload saved values' }).click();
		await expect
			.element(page.getByRole('textbox', { name: 'Camera default offset (ms)' }))
			.toHaveValue('800');
		await expect
			.element(page.getByRole('combobox', { name: 'Main stream mode' }))
			.toHaveValue('inherit');
	});

	it('resets all overrides by sending an empty settings object', async () => {
		const onsave = vi.fn(async () => camera({ recorded_audio: {} }));
		await render(RecordedAudioSettings, { props: { camera: camera(), onsave } });
		await page.getByRole('combobox', { name: 'Camera default mode' }).selectOptions('inherit');
		await page.getByRole('combobox', { name: 'Main stream mode' }).selectOptions('inherit');
		await page.getByRole('button', { name: 'Save recorded timing' }).click();
		expect(onsave).toHaveBeenCalledExactlyOnceWith({}, 'revision-one');
	});

	it('does not offer saves for an older server that omits timing support', async () => {
		const onsave = vi.fn();
		const view = await render(RecordedAudioSettings, {
			props: { camera: camera({ recorded_audio: undefined }), onsave }
		});
		await expect
			.element(page.getByText('Recorded-audio settings are unavailable on this server.'))
			.toBeVisible();
		expect(view.container.querySelector('form')).toBeNull();
		expect(onsave).not.toHaveBeenCalled();
	});

	it.each([320, 390])('fits controls and actions inside a %ipx viewport', async (width) => {
		await page.viewport(width, 850);
		const view = await render(RecordedAudioSettings, {
			props: { camera: camera(), onsave: vi.fn() }
		});
		const form = view.container.querySelector('form')!;
		expect(form.scrollWidth).toBeLessThanOrEqual(width);
		for (const control of view.container.querySelectorAll('input, select, button')) {
			const rect = control.getBoundingClientRect();
			expect(rect.left).toBeGreaterThanOrEqual(0);
			expect(rect.right).toBeLessThanOrEqual(width);
			expect(rect.height).toBeGreaterThanOrEqual(44);
		}
		await page.viewport(1280, 720);
	});
});
