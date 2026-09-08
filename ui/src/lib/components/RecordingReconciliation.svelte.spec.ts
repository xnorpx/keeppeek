import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import { create } from '@bufbuild/protobuf';
import {
	RecordingDriftKind,
	RecordingReconciliationReportSchema,
	RecordingRemedy
} from '$lib/proto/webrtc_pb';
import '../../app.css';
import RecordingReconciliation from './RecordingReconciliation.svelte';

const context = vi.hoisted(() => ({ control: vi.fn() }));
vi.mock('$lib/control-context', () => ({ useControlClient: context.control }));

describe('Catalog index reconciliation', () => {
	it('requires review before rebuilding an explicitly eligible recording index', async () => {
		const report = create(RecordingReconciliationReportSchema, {
			reportId: '1'.repeat(32),
			revision: 1n,
			complete: true,
			inspected: 1,
			items: [
				{
					itemId: '2'.repeat(32),
					recordingId: 'recording',
					kind: RecordingDriftKind.INDEX_MISMATCH,
					remedies: [RecordingRemedy.IGNORE, RecordingRemedy.REINDEX]
				}
			]
		});
		const applied = create(RecordingReconciliationReportSchema, {
			...report,
			items: report.items.map((item) => ({ ...item, appliedRemedy: RecordingRemedy.REINDEX }))
		});
		const client = {
			inspectCatalog: vi.fn().mockResolvedValue(report),
			applyRemedy: vi.fn().mockResolvedValue(applied)
		};
		context.control.mockReturnValue({ recordingMaintenance: client });
		await render(RecordingReconciliation, { props: { enabled: true } });
		await page.getByRole('button', { name: 'Inspect catalog' }).click();
		await page.getByRole('button', { name: 'Rebuild index', exact: true }).click();
		const dialog = page.getByRole('dialog', { name: 'Rebuild this playback index?' });
		await expect.element(dialog).toBeVisible();
		expect(client.applyRemedy).not.toHaveBeenCalled();
		await dialog.getByRole('button', { name: 'Rebuild index', exact: true }).click();
		expect(client.applyRemedy).toHaveBeenCalledWith(
			report,
			'2'.repeat(32),
			RecordingRemedy.REINDEX
		);
		await expect.element(page.getByText('Playback index rebuilt', { exact: true })).toBeVisible();
	});

	it('does not offer reindexing for unknown files', async () => {
		const report = create(RecordingReconciliationReportSchema, {
			reportId: '1'.repeat(32),
			complete: true,
			items: [
				{
					itemId: '2'.repeat(32),
					label: 'unknown.mp4',
					kind: RecordingDriftKind.UNKNOWN_FILE,
					remedies: [RecordingRemedy.IGNORE]
				}
			]
		});
		context.control.mockReturnValue({
			recordingMaintenance: { inspectCatalog: async () => report }
		});
		await render(RecordingReconciliation, { props: { enabled: true } });
		await page.getByRole('button', { name: 'Inspect catalog' }).click();
		await expect.element(page.getByRole('button', { name: 'Acknowledge' })).toBeVisible();
		await expect
			.element(page.getByRole('button', { name: 'Rebuild index' }))
			.not.toBeInTheDocument();
	});
});
