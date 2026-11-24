import { page } from 'vitest/browser';
import { expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { tick } from 'svelte';
import type { AccessConnectionState } from '../access';
import type { ControlClient, MediaExportJob, MediaExportDownload } from '../control-client';
import CompletedExportDownloads from './CompletedExportDownloads.svelte';

const job: MediaExportJob = {
	id: 'original-job',
	sourceId: 'front-door',
	streamId: 'main',
	requestedStartMs: 1000,
	requestedEndMs: 2000,
	alignedStartMs: 1000,
	status: 'ready',
	progress: 1,
	bytesWritten: 5,
	estimatedBytes: 5,
	fileName: 'owner.mp4',
	sha256: null,
	expiresAtMs: null,
	missingRanges: [],
	error: null,
	retryable: false,
	burnInTimestamp: false,
	eventSeed: null
};

function access(id: string, generation: number): AccessConnectionState {
	return {
		status: 'authenticated',
		message: null,
		generation,
		session: {
			id,
			principalId: id,
			displayName: id,
			role: 'user',
			local: true,
			clientClassification: 'local',
			createdAtMs: 0,
			lastActivityAtMs: 1,
			absoluteExpiresAtMs: 9_000_000_000_000,
			credentialExpiresAtMs: null
		}
	};
}

function fixture() {
	let change = (_state: AccessConnectionState) => {};
	const controller = {
		onAccessState(listener: (s: AccessConnectionState) => void) {
			change = listener;
			listener(access('owner', 1));
			return () => {
				change = () => {};
			};
		},
		listExports: vi.fn<ControlClient['listExports']>(),
		downloadExport: vi.fn<ControlClient['downloadExport']>()
	};
	render(CompletedExportDownloads, { controller });
	return { controller, replaceSession: () => change(access('next', 2)) };
}

it('clears late owner results and shows the new session’s empty list', async () => {
	const { controller, replaceSession } = fixture();
	const pending = Promise.withResolvers<MediaExportJob[]>();
	controller.listExports.mockReturnValueOnce(pending.promise).mockResolvedValueOnce([]);
	await page.getByRole('button', { name: 'Find completed exports' }).click();
	replaceSession();
	pending.resolve([job]);
	await tick();
	await page.getByRole('button', { name: 'Find completed exports' }).click();
	await expect
		.element(page.getByRole('status'))
		.toHaveTextContent('No completed exports are available.');
	await expect
		.element(page.getByRole('button', { name: 'Download owner.mp4' }))
		.not.toBeInTheDocument();
});

it('discards a completed download after the requester changes', async () => {
	const { controller, replaceSession } = fixture();
	const saved = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});
	try {
		controller.listExports.mockResolvedValueOnce([job]);
		const pending = Promise.withResolvers<MediaExportDownload>();
		controller.downloadExport.mockReturnValueOnce(pending.promise);
		await page.getByRole('button', { name: 'Find completed exports' }).click();
		await page.getByRole('button', { name: 'Download owner.mp4' }).click();
		expect(controller.downloadExport).toHaveBeenCalledWith('original-job');
		replaceSession();
		pending.resolve({ job, blob: new Blob(['bytes']) });
		await pending.promise;
		await tick();
		expect(saved).not.toHaveBeenCalled();
		await expect
			.element(page.getByRole('button', { name: 'Find completed exports' }))
			.toBeEnabled();
	} finally {
		saved.mockRestore();
	}
});
