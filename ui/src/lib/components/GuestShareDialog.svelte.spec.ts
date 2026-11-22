import { page } from 'vitest/browser';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import GuestShareDialog from './GuestShareDialog.svelte';
import { missingRangeProof } from '../sharing-range';
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

	it('shows the original recording selection next to measured file times and carries it into creation', async () => {
		const client = controller();
		const evidence = shareEvidenceFixture();
		const target = {
			sourceId: evidence.sourceId,
			streamId: 'main' as const,
			startMs: Number(evidence.startMs) - 1000,
			endMs: Number(evidence.endMs) + 1000
		};
		const rangeClient = {
			getServerCapabilities: vi
				.fn()
				.mockResolvedValue({ capabilityIds: ['keeppeek.guest-range.v1'] }),
			listExports: vi.fn().mockResolvedValue([]),
			createExport: vi.fn().mockResolvedValue({ id: evidence.exportJobId, status: 'ready' }),
			getExport: vi.fn(),
			queryStoredTimeline: vi.fn(),
			searchEventMetadata: vi.fn()
		};
		await render(GuestShareDialog, {
			props: { controller: client, range: { target, client: rangeClient }, onclose: vi.fn() }
		});
		await expect
			.element(page.getByText(/Selected interval/))
			.toMatchTextContent('2026-10-17T06:11:47.000Z');
		await expect
			.element(page.getByText('17 Oct 2026, 06:11:48 UTC', { exact: true }))
			.toBeVisible();
		const guard = {
			sourceId: evidence.sourceId,
			startMs: BigInt(target.startMs),
			endMs: BigInt(target.endMs)
		};
		expect(client.previewExport).toHaveBeenCalledWith(evidence.exportJobId, guard);
		await page.getByRole('button', { name: 'Create guest link', exact: true }).click();
		expect(client.create).toHaveBeenCalledWith(
			expect.objectContaining({ recordingRange: guard }),
			sharingFixtureOrigin
		);
	});

	it('tries retained substream footage after a generic MP4 main stream fails codec verification', async () => {
		const evidence = shareEvidenceFixture();
		const target = {
			sourceId: evidence.sourceId,
			startMs: Number(evidence.startMs),
			endMs: Number(evidence.endMs)
		};
		const streams = ['main', 'sub'];
		const ready = {
			id: 'main-file',
			sourceId: target.sourceId,
			streamId: 'main',
			requestedStartMs: target.startMs,
			requestedEndMs: target.endMs,
			status: 'ready',
			expiresAtMs: sharingFixtureNow + 60000
		};
		const rangeClient = {
			getServerCapabilities: vi.fn().mockResolvedValue({
				capabilityIds: ['keeppeek.guest-range.v1'],
				storedMediaSources: [
					{
						sourceId: target.sourceId,
						streams: streams.map((streamId) => ({ streamId, contentType: 'video/mp4' }))
					}
				]
			}),
			queryStoredTimeline: vi
				.fn()
				.mockResolvedValue({ ranges: streams.map((streamId) => ({ ...target, streamId })) }),
			listExports: vi.fn().mockResolvedValue([ready]),
			getExport: vi.fn(),
			createExport: vi.fn().mockResolvedValue({ ...ready, id: 'sub-file', streamId: 'sub' })
		};
		const share = controller();
		share.previewExport.mockRejectedValueOnce(
			new Error('the selected recording could not be verified as browser-compatible')
		);
		await render(GuestShareDialog, {
			props: { controller: share, range: { target, client: rangeClient }, onclose: vi.fn() }
		});
		await expect.element(page.getByLabelText('Display name')).toBeVisible();
		expect(share.previewExport.mock.calls.map(([id]) => id)).toEqual(['main-file', 'sub-file']);
		expect(rangeClient.createExport).toHaveBeenCalledOnce();
		expect(rangeClient.createExport).toHaveBeenCalledWith({
			...target,
			streamId: 'sub',
			allowPartial: true,
			guestRangePreparation: true
		});
	});

	it.each([
		{ failure: 'Connection lost', existing: true, replacement: false },
		{ failure: missingRangeProof, existing: false, replacement: false },
		{ failure: missingRangeProof, existing: true, replacement: true },
		{
			failure: 'recorded audio correction prevents verification of the selected source interval',
			existing: true,
			replacement: true
		}
	])(
		'allows explicit replacement only for a permanent failure on a reused artifact: $failure, existing=$existing',
		async ({ failure, existing, replacement }) => {
			const evidence = shareEvidenceFixture();
			const target = {
				sourceId: evidence.sourceId,
				streamId: 'sub' as const,
				startMs: Number(evidence.startMs),
				endMs: Number(evidence.endMs)
			};
			const old = {
				id: evidence.exportJobId,
				sourceId: target.sourceId,
				streamId: 'sub',
				requestedStartMs: target.startMs,
				requestedEndMs: target.endMs,
				status: 'ready',
				expiresAtMs: sharingFixtureNow + 60_000
			};
			const rangeClient = {
				getServerCapabilities: vi
					.fn()
					.mockResolvedValue({ capabilityIds: ['keeppeek.guest-range.v1'] }),
				queryStoredTimeline: vi.fn(),

				listExports: vi.fn().mockResolvedValue(existing ? [old] : []),
				getExport: vi.fn(),
				createExport: vi.fn().mockResolvedValue({ ...old, id: 'new-artifact' })
			};
			const share = controller();
			share.previewExport.mockRejectedValueOnce(new Error(failure));
			await render(GuestShareDialog, {
				props: { controller: share, range: { target, client: rangeClient }, onclose: vi.fn() }
			});
			await expect.element(page.getByRole('alert')).toMatchTextContent('could not verify');
			const fresh = page.getByRole('button', { name: 'Prepare a fresh export' });
			const reload = page.getByRole('button', { name: 'Reload preview' });
			await expect.element(reload).toBeVisible();
			if (replacement) await fresh.click();
			else {
				await expect.element(fresh).not.toBeInTheDocument();
				await reload.click();
			}
			await expect.element(page.getByLabelText('Display name')).toBeVisible();
			expect(rangeClient.createExport).toHaveBeenCalledTimes(!existing || replacement ? 1 : 0);
			expect(rangeClient.listExports).toHaveBeenCalledTimes(1);
			expect(share.previewExport).toHaveBeenCalledTimes(2);
		}
	);

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
