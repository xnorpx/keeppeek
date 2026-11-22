import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import GuestShareGrants from './GuestShareGrants.svelte';
import { shareGrantFixture, shareListFixture, sharingFixtureRequestId } from '../sharing.fixture';
import { ShareGrantStatus } from '../proto/webrtc_pb';

function controller() {
	return {
		list: vi.fn().mockResolvedValue(shareListFixture()),
		revoke: vi.fn().mockResolvedValue({ ...shareGrantFixture(), status: ShareGrantStatus.REVOKED }),
		revokeEvidence: vi.fn().mockResolvedValue(1)
	};
}

describe('sanitized guest grant management', () => {
	it('confirms immutable evidence scope before a bulk revocation and requires a fresh list afterward', async () => {
		const client = controller();
		await render(GuestShareGrants, { props: { controller: client } });
		await page.getByText('Audit and evidence revocation', { exact: true }).click();
		await page.getByRole('button', { name: 'Revoke all links to this clip', exact: true }).click();
		await expect.element(page.getByRole('dialog')).toMatchTextContent('every active guest link');
		expect(client.revokeEvidence).not.toHaveBeenCalled();
		await page.getByRole('button', { name: 'Confirm revocation', exact: true }).click();
		expect(client.revokeEvidence).toHaveBeenCalledWith(
			{ artifactId: 'artifact-132' },
			'No longer needed'
		);
		await expect
			.element(page.getByRole('button', { name: 'Revoke link', exact: true }))
			.toBeDisabled();
		await page.getByRole('button', { name: 'Reload guest links', exact: true }).click();
		expect(client.list).toHaveBeenCalledTimes(2);
	});
	it('replaces a page instead of accumulating unbounded grant rows', async () => {
		const client = controller();
		const next = '00000000-0000-4000-8000-000000000134';
		client.list.mockResolvedValueOnce({ ...shareListFixture(), nextAfterGrantId: next });
		await render(GuestShareGrants, { props: { controller: client } });
		await page.getByRole('button', { name: 'Next page', exact: true }).click();
		expect(client.list).toHaveBeenLastCalledWith({
			pageSize: 50,
			afterGrantId: next,
			allCreators: false
		});
		await expect
			.element(page.getByRole('heading', { name: 'Front step visit', exact: true }))
			.toBeVisible();
	});

	it('loads bounded creator pages without a recover/copy bearer action', async () => {
		const client = controller();
		await render(GuestShareGrants, { props: { controller: client } });
		await expect
			.element(page.getByRole('heading', { name: 'Front step visit', exact: true }))
			.toBeVisible();
		expect(client.list).toHaveBeenCalledWith({
			pageSize: 50,
			afterGrantId: undefined,
			allCreators: false
		});
		await expect.element(page.getByRole('button', { name: /Copy/ })).not.toBeInTheDocument();
		expect(document.querySelector('a[href*="/share#"]')).toBeNull();
	});
	it('requires deliberate confirmation and the exact displayed grant revision', async () => {
		const client = controller();
		await render(GuestShareGrants, { props: { controller: client } });
		await page.getByRole('button', { name: 'Revoke link', exact: true }).click();
		await expect.element(page.getByRole('dialog', { name: 'Revoke guest access' })).toBeVisible();
		await expect.element(page.getByRole('button', { name: 'Cancel', exact: true })).toHaveFocus();
		expect(client.revoke).not.toHaveBeenCalled();
		await page.getByRole('button', { name: 'Confirm revocation', exact: true }).click();
		expect(client.revoke).toHaveBeenCalledWith(
			expect.objectContaining({ grantId: sharingFixtureRequestId, revision: 1n }),
			'No longer needed'
		);
	});
	it('shows failed mutations without replacing the displayed row and supports explicit reload', async () => {
		const client = controller();
		client.revoke.mockRejectedValueOnce(new Error('Revision conflict'));
		await render(GuestShareGrants, { props: { controller: client } });
		await page.getByRole('button', { name: 'Revoke link', exact: true }).click();
		await page.getByRole('button', { name: 'Confirm revocation', exact: true }).click();
		await expect.element(page.getByRole('alert')).toMatchTextContent('could not be confirmed');
		await expect
			.element(page.getByRole('heading', { name: 'Front step visit', exact: true }))
			.toBeVisible();
		await page.getByRole('button', { name: 'Cancel', exact: true }).click();
		await page.getByRole('button', { name: 'Reload guest links', exact: true }).click();
		expect(client.list).toHaveBeenCalledTimes(2);
	});
});
