import { page, userEvent } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import type { ServerLogEntry } from '$lib/types';
import LogViewer from './LogViewer.svelte';

function entry(
	sequence: number,
	level: ServerLogEntry['level'],
	target: string,
	message: string
): ServerLogEntry {
	return {
		sequence,
		timestamp_ms: Date.UTC(2026, 7, 12, 12, 0, sequence),
		level,
		target,
		message,
		fields: {}
	};
}

describe('LogViewer', () => {
	it('filters entries by level, target, and message text', async () => {
		render(LogViewer, {
			props: {
				entries: [
					entry(1, 'info', 'keeppeek::storage', 'recording started'),
					entry(2, 'warn', 'str0m', 'packet queue full')
				],
				onclear: vi.fn(),
				ondownload: vi.fn()
			}
		});

		await userEvent.click(page.getByLabelText('Info'));
		await expect.element(page.getByText('recording started')).not.toBeInTheDocument();
		await expect.element(page.getByText('packet queue full')).toBeVisible();

		await userEvent.fill(page.getByLabelText('Target filter'), 'keeppeek');
		await expect.element(page.getByText('packet queue full')).not.toBeInTheDocument();

		await userEvent.click(page.getByLabelText('Info'));
		await userEvent.fill(page.getByLabelText('Text filter'), 'started');
		await expect.element(page.getByText('recording started')).toBeVisible();
	});

	it('holds incoming entries while paused and flushes them on resume', async () => {
		const onclear = vi.fn();
		const ondownload = vi.fn();
		const view = render(LogViewer, {
			props: {
				entries: [entry(1, 'info', 'keeppeek::test', 'first')],
				onclear,
				ondownload
			}
		});
		await userEvent.click(page.getByRole('button', { name: 'Pause' }));

		await view.rerender({
			entries: [
				entry(1, 'info', 'keeppeek::test', 'first'),
				entry(2, 'error', 'keeppeek::test', 'second')
			],
			onclear,
			ondownload
		});

		await expect.element(page.getByText('second')).not.toBeInTheDocument();
		await userEvent.click(page.getByRole('button', { name: 'Resume (1 unread)' }));
		await expect.element(page.getByText('second')).toBeVisible();
	});

	it('clears the visible view through its callback', async () => {
		const onclear = vi.fn();
		render(LogViewer, {
			props: {
				entries: [entry(1, 'error', 'browser.error', 'failed')],
				onclear,
				ondownload: vi.fn()
			}
		});

		await userEvent.click(page.getByRole('button', { name: 'Clear log view' }));

		expect(onclear).toHaveBeenCalledOnce();
		await expect.element(page.getByText('failed')).not.toBeInTheDocument();
		await expect.element(page.getByText('No logs match the current filters.')).toBeVisible();
	});
});
