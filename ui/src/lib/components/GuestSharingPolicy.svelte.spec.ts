import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import GuestSharingPolicy from './GuestSharingPolicy.svelte';
import { sharePolicyFixture, sharingFixtureOrigin } from '../sharing.fixture';

function controller() {
	return {
		getPolicy: vi.fn().mockResolvedValue(sharePolicyFixture()),
		configurePolicy: vi.fn().mockResolvedValue({ ...sharePolicyFixture(), revision: 'policy-2' })
	};
}

describe('guest sharing deployment policy', () => {
	it('sends the displayed revision with the canonical HTTPS origin', async () => {
		const client = controller();
		await render(GuestSharingPolicy, { props: { controller: client } });
		await expect
			.element(page.getByLabelText('External HTTPS origin'))
			.toHaveValue(sharingFixtureOrigin);
		await page.getByRole('button', { name: 'Save sharing policy', exact: true }).click();
		expect(client.configurePolicy).toHaveBeenCalledWith(
			{ enabled: true, publicOrigin: sharingFixtureOrigin },
			'policy-1'
		);
		await expect.element(page.getByRole('status')).toMatchTextContent('saved');
	});
	it('rejects path-bearing origins before writing and preserves a failed draft', async () => {
		const client = controller();
		await render(GuestSharingPolicy, { props: { controller: client } });
		await page.getByLabelText('External HTTPS origin').fill(`${sharingFixtureOrigin}/wrong`);
		await page.getByRole('button', { name: 'Save sharing policy', exact: true }).click();
		await expect.element(page.getByRole('alert')).toMatchTextContent('without a path');
		expect(client.configurePolicy).not.toHaveBeenCalled();
		await page.getByLabelText('External HTTPS origin').fill('https://another.example');
		client.configurePolicy.mockRejectedValueOnce(new Error('Save failed'));
		await page.getByRole('button', { name: 'Save sharing policy', exact: true }).click();
		await expect.element(page.getByRole('alert')).toMatchTextContent('not confirmed');
		await expect
			.element(page.getByLabelText('External HTTPS origin'))
			.toHaveValue('https://another.example');
	});
});
