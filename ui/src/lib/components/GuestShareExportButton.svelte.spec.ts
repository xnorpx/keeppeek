import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import GuestShareExportButton from './GuestShareExportButton.svelte';
import type { AccessConnectionState } from '../access';
import { shareEvidenceFixture, sharePolicyFixture, shareIssuedFixture } from '../sharing.fixture';

function access(): AccessConnectionState {
	return {
		status: 'authenticated',
		message: null,
		generation: 1,
		session: {
			id: 'ordinary-auth-session',
			principalId: 'creator-a',
			displayName: 'Administrator',
			role: 'administrator',
			local: true,
			clientClassification: 'local',
			createdAtMs: 1,
			lastActivityAtMs: 1,
			absoluteExpiresAtMs: 9_000_000_000_000,
			credentialExpiresAtMs: null
		}
	};
}
function controller(initial = access()) {
	let notify: (state: AccessConnectionState) => void = () => {};
	const sharing = {
		getPolicy: vi.fn().mockResolvedValue(sharePolicyFixture()),
		previewExport: vi.fn().mockResolvedValue(shareEvidenceFixture()),
		create: vi.fn().mockResolvedValue(shareIssuedFixture())
	};
	return {
		sharing,
		onAccessState: vi.fn((listener: (state: AccessConnectionState) => void) => {
			notify = listener;
			listener(initial);
			return () => {};
		}),
		changeAccess: (state: AccessConnectionState) => notify(state)
	};
}

describe('ready export guest creation entry', () => {
	it('never offers creation to an ordinary User', async () => {
		const state = access();
		state.session!.role = 'user';
		const client = controller(state);
		await render(GuestShareExportButton, {
			props: { controller: client, exportJobId: 'export-ready-132' }
		});
		await expect
			.element(page.getByRole('button', { name: 'Create guest link', exact: true }))
			.not.toBeInTheDocument();
		expect(client.sharing.previewExport).not.toHaveBeenCalled();
	});
	it('destroys the creator dialog as soon as the authenticated principal changes', async () => {
		const client = controller();
		await render(GuestShareExportButton, {
			props: { controller: client, exportJobId: 'export-ready-132' }
		});
		await page.getByRole('button', { name: 'Create guest link', exact: true }).click();
		await expect.element(page.getByRole('dialog', { name: 'Create guest link' })).toBeVisible();
		const changed = access();
		changed.generation = 2;
		changed.session!.principalId = 'creator-b';
		client.changeAccess(changed);
		await expect
			.element(page.getByRole('dialog', { name: 'Create guest link' }))
			.not.toBeInTheDocument();
		expect(client.sharing.create).not.toHaveBeenCalled();
	});
});
