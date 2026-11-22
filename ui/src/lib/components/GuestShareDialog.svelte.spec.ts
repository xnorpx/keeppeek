import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import GuestShareDialog from './GuestShareDialog.svelte';
import {
	shareEvidenceFixture,
	sharePolicyFixture,
	shareIssuedFixture,
	sharingFixtureNow,
	sharingFixtureOrigin,
	sharingFixtureToken,
	sharingFixtureRequestId
} from '../sharing.fixture';

function controller() {
	return {
		getPolicy: vi.fn().mockResolvedValue(sharePolicyFixture()),
		previewExport: vi.fn().mockResolvedValue(shareEvidenceFixture()),
		create: vi.fn().mockResolvedValue(shareIssuedFixture())
	};
}
beforeEach(() => {
	vi.spyOn(Date, 'now').mockReturnValue(sharingFixtureNow);
	vi.spyOn(crypto, 'randomUUID').mockReturnValue(sharingFixtureRequestId);
});
afterEach(() => vi.restoreAllMocks());

describe('creator guest share dialog', () => {
	it('closes and forgets a disclosed secret before a document can enter the back-forward cache', async () => {
		const client = controller();
		const close = vi.fn();
		await render(GuestShareDialog, {
			props: { controller: client, exportJobId: 'export-ready-132', onclose: close }
		});
		await page.getByRole('button', { name: 'Create guest link', exact: true }).click();
		await expect.element(page.getByLabelText('One-time guest link')).toBeVisible();
		window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true }));
		expect(close).toHaveBeenCalledOnce();
		expect(document.querySelector('dialog')?.open).toBe(false);
		await expect.element(page.getByLabelText('One-time guest link')).not.toBeInTheDocument();
		expect(document.body.textContent).not.toContain(sharingFixtureToken);
	});

	it('previews the full aligned MP4 and sends its immutable identity and least-privilege defaults', async () => {
		const client = controller();
		await render(GuestShareDialog, {
			props: { controller: client, exportJobId: 'export-ready-132', onclose: vi.fn() }
		});
		await expect
			.element(page.getByText('17 Oct 2026, 06:11:48 UTC', { exact: true }))
			.toBeVisible();
		await expect.element(page.getByText(/complete MP4/)).toBeVisible();
		await expect
			.element(page.getByRole('checkbox', { name: 'Allow playback', exact: true }))
			.toBeChecked();
		await expect
			.element(page.getByRole('checkbox', { name: 'Allow download', exact: true }))
			.not.toBeChecked();
		await page.getByLabelText('Display name').fill('Front step visit');
		await page.getByLabelText('Maximum views').fill('2');
		await page.getByRole('button', { name: 'Create guest link', exact: true }).click();
		expect(client.create).toHaveBeenCalledWith(
			expect.objectContaining({
				requestId: sharingFixtureRequestId,
				artifactId: 'artifact-132',
				expectedSha256: 'a'.repeat(64),
				allowStream: true,
				allowDownload: false,
				maxViews: 2
			}),
			sharingFixtureOrigin
		);
		await expect
			.element(page.getByLabelText('One-time guest link'))
			.toHaveValue(`${sharingFixtureOrigin}/share#${sharingFixtureToken}`);
		await expect.element(page.getByText(/Copy this link now/)).toBeVisible();
		expect(document.querySelector(`a[href*="${sharingFixtureToken}"]`)).toBeNull();
	});

	it('requires list/revoke recovery after an unconfirmed create and never retries automatically', async () => {
		const client = controller();
		client.create.mockRejectedValueOnce(new Error('Connection lost'));
		await render(GuestShareDialog, {
			props: { controller: client, exportJobId: 'export-ready-132', onclose: vi.fn() }
		});
		await page.getByRole('button', { name: 'Create guest link', exact: true }).click();
		await expect.element(page.getByRole('alert')).toMatchTextContent('not confirmed');
		await expect
			.element(page.getByRole('link', { name: 'Review guest links', exact: true }))
			.toHaveAttribute('href', '/settings#access');
		await expect
			.element(page.getByRole('button', { name: 'Create guest link', exact: true }))
			.not.toBeInTheDocument();
		expect(client.create).toHaveBeenCalledOnce();
	});

	it('drops a late creation secret after the dialog is closed', async () => {
		const client = controller();
		const pending = Promise.withResolvers<ReturnType<typeof shareIssuedFixture>>();
		client.create.mockReturnValueOnce(pending.promise);
		const screen = await render(GuestShareDialog, {
			props: { controller: client, exportJobId: 'export-ready-132', onclose: vi.fn() }
		});
		await page.getByRole('button', { name: 'Create guest link', exact: true }).click();
		await screen.unmount();
		pending.resolve(shareIssuedFixture());
		await Promise.resolve();
		expect(document.body.textContent).not.toContain(sharingFixtureToken);
		expect(
			[...Object.values(sessionStorage), ...Object.values(localStorage)].some((value) =>
				String(value).includes(sharingFixtureToken)
			)
		).toBe(false);
	});

	it('keeps creation unavailable when policy or provenance preview fails and offers a real reload', async () => {
		const client = controller();
		client.previewExport.mockRejectedValueOnce(new Error('Legacy export'));
		await render(GuestShareDialog, {
			props: { controller: client, exportJobId: 'export-ready-132', onclose: vi.fn() }
		});
		await expect.element(page.getByRole('alert')).toMatchTextContent('could not be previewed');
		await page.getByRole('button', { name: 'Reload preview', exact: true }).click();
		await expect
			.element(page.getByRole('button', { name: 'Create guest link', exact: true }))
			.toBeEnabled();
		expect(client.previewExport).toHaveBeenCalledTimes(2);
		expect(client.create).not.toHaveBeenCalled();
	});
});
