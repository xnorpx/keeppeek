import { create } from '@bufbuild/protobuf';
import { page, userEvent } from 'vitest/browser';
import { expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import {
	CameraRecordingMode,
	RecordingControlReason,
	RecordingControlStateSchema,
	RecordingOverrideSource,
	RecordingOverrideStateSchema,
	type RecordingControlState
} from '$lib/proto/webrtc_pb';
import type { ControlClient } from '$lib/control-client';
import RecordingControlPanel from './RecordingControlPanel.svelte';
import '../../app.css';

function controller(supported = true, administrator = true) {
	const state = create(RecordingControlStateSchema, {
		sourceId: 'camera',
		revision: 'epoch:1',
		configuredMode: CameraRecordingMode.SUB,
		effectiveMode: CameraRecordingMode.SUB,
		reason: RecordingControlReason.CONFIGURATION
	});
	const get = vi.fn(async () => state);
	const pause = vi.fn(async (): Promise<RecordingControlState> => {
		throw new Error('Revision changed; refresh state.');
	});
	const clear = vi.fn(async () => state);
	return {
		recordingControl: { get, pause, clear },
		onCapabilities(listener: Parameters<ControlClient['onCapabilities']>[0]) {
			listener(supported ? ['keeppeek.recording-control.v1'] : []);
			return () => {};
		},
		onAccessState(listener: Parameters<ControlClient['onAccessState']>[0]) {
			listener({
				status: 'authenticated',
				generation: 1,
				message: null,
				session: {
					id: 'session',
					principalId: 'admin',
					displayName: 'Admin',
					role: administrator ? 'administrator' : 'user',
					local: true,
					clientClassification: 'local',
					createdAtMs: 0,
					lastActivityAtMs: 0,
					absoluteExpiresAtMs: 0,
					credentialExpiresAtMs: null
				}
			});
			return () => {};
		}
	};
}

it('preserves the pause draft after failure and allows refreshing current state', async () => {
	const client = controller();
	await render(RecordingControlPanel, { controller: client, sourceId: 'camera' });
	await expect.element(page.getByText('Configured: Sub')).toBeVisible();
	await page.getByLabelText('Reason for pause').fill('inspection');
	await page.getByLabelText('Pause duration (minutes)').fill('15');
	await page.getByRole('button', { name: 'Pause recording' }).click();
	await expect
		.element(page.getByRole('alert'))
		.toHaveTextContent('Revision changed; refresh state.');
	await expect.element(page.getByLabelText('Reason for pause')).toHaveValue('inspection');
	await expect.element(page.getByLabelText('Pause duration (minutes)')).toHaveValue(15);
	expect(client.recordingControl.pause).toHaveBeenCalledWith(
		expect.objectContaining({ revision: 'epoch:1' }),
		'inspection',
		900_000
	);
	await page.getByRole('button', { name: 'Refresh recording state' }).click();
	await expect.element(page.getByText('Configured: Sub')).toBeVisible();
});

it('does not request administrator state for a user account', async () => {
	const client = controller(true, false);
	await render(RecordingControlPanel, { controller: client, sourceId: 'camera' });
	await expect
		.element(page.getByText('An administrator account is required to manage recording controls.'))
		.toBeVisible();
	expect(client.recordingControl.get).not.toHaveBeenCalled();
});

it('submits by keyboard and displays the authoritative pause and clear result', async () => {
	const client = controller();
	const configured = await client.recordingControl.get();
	client.recordingControl.pause.mockResolvedValue(
		create(RecordingControlStateSchema, {
			...configured,
			revision: 'epoch:2',
			effectiveMode: CameraRecordingMode.OFF,
			reason: RecordingControlReason.OVERRIDE,
			overrideState: create(RecordingOverrideStateSchema, {
				enabled: false,
				actor: 'Admin',
				reason: 'inspection',
				source: RecordingOverrideSource.MANUAL,
				expiresAtMs: 1_800_000_000_000n
			})
		})
	);
	await render(RecordingControlPanel, { controller: client, sourceId: 'camera' });
	await expect.element(page.getByText('Configured: Sub')).toBeVisible();
	await page.getByLabelText('Reason for pause').fill('inspection');
	await userEvent.keyboard('{Enter}');
	await expect.element(page.getByText('Effective: Off')).toBeVisible();
	await expect.element(page.getByText('Requested by Admin (manual)')).toBeVisible();
	await page.getByRole('button', { name: 'End temporary override' }).click();
	expect(client.recordingControl.clear).toHaveBeenCalledWith(
		expect.objectContaining({ revision: 'epoch:2' })
	);
	await expect.element(page.getByText('Effective: Sub')).toBeVisible();
});

it('ignores a pending response after capability loss', async () => {
	const client = controller();
	const configured = await client.recordingControl.get();
	let resolve!: (value: RecordingControlState) => void;
	client.recordingControl.get.mockImplementation(
		() =>
			new Promise((done) => {
				resolve = done;
			})
	);
	let capabilities!: Parameters<ControlClient['onCapabilities']>[0];
	client.onCapabilities = (listener) => {
		capabilities = listener;
		listener(['keeppeek.recording-control.v1']);
		return () => {};
	};
	await render(RecordingControlPanel, { controller: client, sourceId: 'camera' });
	await expect.element(page.getByText('Loading recording state…')).toBeVisible();
	capabilities([]);
	resolve(configured);
	await expect
		.element(page.getByText('Temporary recording controls are unavailable on this server.'))
		.toBeVisible();
	await expect.element(page.getByText('Configured: Sub')).not.toBeInTheDocument();
});

it('ignores old source responses after the camera changes', async () => {
	const client = controller();
	const configured = await client.recordingControl.get();
	let resolve!: (value: RecordingControlState) => void;
	client.recordingControl.get.mockImplementationOnce(
		() =>
			new Promise((done) => {
				resolve = done;
			})
	);
	client.recordingControl.get.mockResolvedValue(
		create(RecordingControlStateSchema, {
			...configured,
			sourceId: 'other',
			configuredMode: CameraRecordingMode.MAIN,
			effectiveMode: CameraRecordingMode.MAIN
		})
	);
	const view = await render(RecordingControlPanel, { controller: client, sourceId: 'camera' });
	await expect.element(page.getByText('Loading recording state…')).toBeVisible();
	await view.rerender({ sourceId: 'other' });
	await expect.element(page.getByText('Configured: Main')).toBeVisible();
	resolve(configured);
	await expect.element(page.getByText('Configured: Sub')).not.toBeInTheDocument();
});

it('discards a pending mutation after administrator access is lost', async () => {
	const client = controller();
	const configured = await client.recordingControl.get();
	let resolve!: (value: RecordingControlState) => void;
	client.recordingControl.pause.mockImplementation(
		() =>
			new Promise((done) => {
				resolve = done;
			})
	);
	let access!: Parameters<ControlClient['onAccessState']>[0];
	const subscribe = client.onAccessState;
	client.onAccessState = (listener) => {
		access = listener;
		return subscribe(listener);
	};
	await render(RecordingControlPanel, { controller: client, sourceId: 'camera' });
	await expect.element(page.getByText('Configured: Sub')).toBeVisible();
	await page.getByLabelText('Reason for pause').fill('inspection');
	await page.getByRole('button', { name: 'Pause recording' }).click();
	controller(true, false).onAccessState(access);
	resolve(configured);
	await expect
		.element(page.getByText('An administrator account is required to manage recording controls.'))
		.toBeVisible();
	await expect
		.element(page.getByRole('button', { name: 'Pause recording' }))
		.not.toBeInTheDocument();
});

it('does not request unsupported controls', async () => {
	const client = controller(false);
	await render(RecordingControlPanel, { controller: client, sourceId: 'camera' });
	await expect
		.element(page.getByText('Temporary recording controls are unavailable on this server.'))
		.toBeVisible();
	expect(client.recordingControl.get).not.toHaveBeenCalled();
	await expect
		.element(page.getByRole('button', { name: 'Pause recording' }))
		.not.toBeInTheDocument();
});

it('keeps controls within narrow and desktop viewports', async () => {
	const client = controller();
	await render(RecordingControlPanel, { controller: client, sourceId: 'camera' });
	await expect.element(page.getByText('Configured: Sub')).toBeVisible();
	try {
		for (const width of [320, 768, 1024, 1440]) {
			await page.viewport(width, 900);
			expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
			await expect.element(page.getByRole('button', { name: 'Pause recording' })).toBeVisible();
			await page.screenshot({ path: `../../../test-results/recording-control-${width}.png` });
		}
	} finally {
		await page.viewport(1280, 720);
	}
});
