import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import CameraConfigurationEditor from './CameraConfigurationEditor.svelte';
import type { CameraSettings } from '$lib/types';
import '../../app.css';

const camera: CameraSettings = {
	id: 'gate',
	ip: '192.0.2.1',
	display_name: 'Gate',
	manufacturer_override: null,
	username_configured: true,
	password_configured: true,
	onvif_port: null,
	http_port: null,
	main_rtsp_url: null,
	sub_rtsp_url: null,
	uid_configured: false,
	backend: 'auto',
	transport: 'tcp',
	record_generic_motion_events: false,
	recording_mode: 'event-only',
	event_recording_duration_secs: 60,
	event_pre_recording_duration_secs: 5,
	event_recording_stream: 'main',
	health: 'healthy',
	model: null
};

describe('event recording camera editor', () => {
	it.each([320, 768, 1024, 1440])('validates and saves with keyboard at %ipx', async (width) => {
		await page.viewport(width, 900);
		const onsave = vi.fn();
		const { container } = await render(CameraConfigurationEditor, {
			camera,
			preRecordingSupported: true,
			onsave,
			oncancel: vi.fn()
		});
		const pre = page.getByRole('textbox', { name: 'Pre-recording duration (seconds)' });
		await pre.fill('31');
		await page.getByRole('button', { name: 'Save camera settings' }).click();
		await expect
			.element(page.getByRole('alert'))
			.toHaveTextContent('Pre-recording duration must be a whole number from 0 to 30 seconds.');
		await expect.element(page.getByRole('alert')).toHaveFocus();
		expect(onsave).not.toHaveBeenCalled();
		await pre.fill('0');
		await page.getByRole('combobox', { name: 'Event recording stream' }).selectOptions('sub');
		await page.getByRole('button', { name: 'Save camera settings' }).click();
		expect(onsave).toHaveBeenLastCalledWith(
			expect.objectContaining({
				recording_mode: 'event-only',
				event_pre_recording_duration_secs: 0,
				event_recording_stream: 'sub',
				event_recording_duration_secs: 60
			})
		);
		await pre.click();
		await userEvent.keyboard('{Tab}');
		await expect
			.element(page.getByRole('combobox', { name: 'Event recording stream' }))
			.toHaveFocus();
		expect(container.scrollWidth).toBeLessThanOrEqual(width);
	});

	it('hides event controls for continuous recording and preserves saved event settings', async () => {
		const onsave = vi.fn();
		await render(CameraConfigurationEditor, {
			camera,
			preRecordingSupported: true,
			onsave,
			oncancel: vi.fn()
		});
		await page.getByRole('combobox', { name: 'Recording mode' }).selectOptions('sub');
		await expect
			.element(page.getByRole('textbox', { name: 'Pre-recording duration (seconds)' }))
			.not.toBeInTheDocument();
		await page.getByRole('button', { name: 'Save camera settings' }).click();
		expect(onsave).toHaveBeenCalledWith(
			expect.objectContaining({
				recording_mode: 'sub',
				event_pre_recording_duration_secs: 5,
				event_recording_stream: 'main'
			})
		);
	});
	it('preserves legacy editing when the server does not advertise pre-recording', async () => {
		const onsave = vi.fn();
		await render(CameraConfigurationEditor, {
			camera: { ...camera, recording_mode: 'event-boost' },
			onsave,
			oncancel: vi.fn()
		});
		await expect
			.element(page.getByRole('textbox', { name: 'Pre-recording duration (seconds)' }))
			.not.toBeInTheDocument();
		await expect
			.element(page.getByRole('textbox', { name: 'Main recording after an event (seconds)' }))
			.toBeVisible();
		await page.getByRole('button', { name: 'Save camera settings' }).click();
		expect(onsave).toHaveBeenCalledOnce();
		expect(onsave.mock.calls[0]?.[0]).not.toHaveProperty('event_pre_recording_duration_secs');
		expect(onsave.mock.calls[0]?.[0]).not.toHaveProperty('event_recording_stream');
	});
});
