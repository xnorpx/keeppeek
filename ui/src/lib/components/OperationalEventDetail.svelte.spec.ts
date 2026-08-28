import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import OperationalEventDetail from './OperationalEventDetail.svelte';

describe('OperationalEventDetail', () => {
	it('shows durable outage evidence and recovery details', async () => {
		const onclose = vi.fn();
		await render(OperationalEventDetail, {
			props: {
				event: {
					id: 'operational-1',
					revision: 3,
					source_id: 'front-door',
					source: 'keeppeek',
					kind: 'recording_interrupted',
					start_time_ms: Date.UTC(2026, 7, 10, 12),
					end_time_ms: Date.UTC(2026, 7, 10, 12, 2),
					confidence: null,
					bbox: null,
					zone: null,
					thumbnail_url: null,
					operational: {
						kind: 'recording_interrupted',
						severity: 'critical',
						cause: 'recording_not_progressing',
						explanation: 'Requested recording writes are not progressing',
						affected_streams: ['main'],
						recording_interrupted: true,
						evidence_source: 'recording_writer',
						stream_id: 'main',
						duration_ms: 120_000,
						recovered: true
					}
				},
				onclose
			}
		});

		await expect.element(page.getByText('Recording interrupted', { exact: true })).toBeVisible();
		await expect.element(page.getByText('RECOVERED', { exact: true })).toBeVisible();
		await expect.element(page.getByText('2m 0s', { exact: true })).toBeVisible();
		await expect.element(page.getByText('main', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Interrupted', { exact: true })).toBeVisible();
		await expect.element(page.getByText('Recording writer', { exact: true })).toBeVisible();
		await expect.element(page.getByText(/recording_not_progressing/)).toBeVisible();

		await userEvent.click(page.getByRole('button', { name: 'Close operational event details' }));
		expect(onclose).toHaveBeenCalledOnce();
	});
});
