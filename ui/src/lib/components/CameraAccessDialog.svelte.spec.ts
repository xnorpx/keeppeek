import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import CameraAccessDialog from './CameraAccessDialog.svelte';
import { cameraAccessCapability } from '$lib/control-client-camera-access';

const credential = { id: '11111111-1111-4111-8111-111111111111', name: 'Viewer' };
const initial = {
	credentialId: credential.id,
	allCameras: false,
	groupIds: [],
	cameraIds: [],
	availableGroupIds: ['outdoor', 'indoor'],
	revision: 1n
};

function controller() {
	let notify: (ids: readonly string[]) => void = () => {};
	return {
		getCameras: vi.fn().mockResolvedValue([
			{ id: '192.0.2.10', name: 'Front door' },
			{ id: '192.0.2.11', name: 'Back door' }
		]),
		getCameraAccess: vi.fn().mockResolvedValue(initial),
		saveCameraAccess: vi
			.fn()
			.mockImplementation(async (settings) => ({ ...settings, revision: 2n })),
		onCapabilities: vi.fn((listener: (ids: readonly string[]) => void) => {
			notify = listener;
			listener([cameraAccessCapability]);
			return () => {};
		}),
		removeCapability: () => notify([])
	};
}

describe('Camera access dialog', () => {
	it('defaults to everything and edits group and camera grants on the selected user', async () => {
		const client = controller();
		client.getCameraAccess.mockResolvedValueOnce({ ...initial, allCameras: true });
		await render(CameraAccessDialog, {
			props: { credential, controller: client, onclose: vi.fn(), onsaved: vi.fn() }
		});
		await expect.element(page.getByRole('dialog', { name: 'User access' })).toBeVisible();
		await expect
			.element(page.getByRole('radio', { name: 'Everything', exact: true }))
			.toBeChecked();
		await page.getByRole('radio', { name: 'Selected groups and cameras' }).click();
		await page.getByRole('checkbox', { name: 'outdoor', exact: true }).click();
		await page.getByRole('checkbox', { name: 'Back door', exact: true }).click();
		await page.getByRole('button', { name: 'Save access', exact: true }).click();
		expect(client.saveCameraAccess).toHaveBeenCalledWith({
			...initial,
			groupIds: ['outdoor'],
			cameraIds: ['192.0.2.11']
		});
	});

	it('loads explicit grants and saves only selected cameras', async () => {
		const client = controller();
		const close = vi.fn();
		await render(CameraAccessDialog, {
			props: { credential, controller: client, onclose: close, onsaved: vi.fn() }
		});
		await expect.element(page.getByRole('dialog', { name: 'User access' })).toBeVisible();
		await expect
			.element(page.getByRole('radio', { name: 'Everything', exact: true }))
			.not.toBeChecked();
		await page.getByRole('checkbox', { name: 'Front door', exact: true }).click();
		await page.getByRole('button', { name: 'Save access', exact: true }).click();
		expect(client.saveCameraAccess).toHaveBeenCalledWith({ ...initial, cameraIds: ['192.0.2.10'] });
		expect(close).toHaveBeenCalledOnce();
	});

	it('preserves selected cameras after a failed save and offers an explicit reload', async () => {
		const client = controller();
		client.saveCameraAccess.mockRejectedValueOnce(
			new Error('Camera access changed; reload before saving.')
		);
		const close = vi.fn();
		await render(CameraAccessDialog, {
			props: { credential, controller: client, onclose: close, onsaved: vi.fn() }
		});
		await page.getByRole('checkbox', { name: 'Back door', exact: true }).click();
		await page.getByRole('button', { name: 'Save access', exact: true }).click();
		await expect.element(page.getByRole('alert')).toHaveTextContent('reload before saving');
		await expect
			.element(page.getByRole('checkbox', { name: 'Back door', exact: true }))
			.toBeChecked();
		expect(close).not.toHaveBeenCalled();
		await page.getByRole('button', { name: 'Reload permissions' }).click();
		await expect
			.element(page.getByRole('checkbox', { name: 'Back door', exact: true }))
			.not.toBeChecked();
	});

	it('disables writes when the capability disappears without losing the draft', async () => {
		const client = controller();
		await render(CameraAccessDialog, {
			props: { credential, controller: client, onclose: vi.fn(), onsaved: vi.fn() }
		});
		await page.getByRole('checkbox', { name: 'Front door', exact: true }).click();
		client.removeCapability();
		await expect
			.element(page.getByRole('button', { name: 'Save access', exact: true }))
			.toBeDisabled();
		await expect
			.element(page.getByRole('checkbox', { name: 'Front door', exact: true }))
			.toBeChecked();
		expect(client.saveCameraAccess).not.toHaveBeenCalled();
	});
});
