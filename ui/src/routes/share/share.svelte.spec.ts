import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { tick } from 'svelte';
import { render } from 'vitest-browser-svelte';
import type { GuestHandoff } from '$lib/guest-api';
import '../../app.css';
import Share from './+page.svelte';

const session = '76c391c5-4a31-4d58-8510-59c62fa0cf29';
const otherSession = '08a105c4-7ce8-48b8-83d0-8085c0ddc92e';
const ready = {
	status: 'ready',
	session_id: session,
	display_name: 'Front step visit',
	start_ms: 1_000,
	end_ms: 61_000,
	expires_at_ms: 5_000_000,
	can_stream: true,
	can_download: false,
	bytes: 24_600_000,
	has_gaps: false
};

function handoff(value: GuestHandoff) {
	Object.defineProperty(window, '__keeppeekTakeGuestToken', {
		configurable: true,
		value: () => value
	});
}

afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
	delete window.__keeppeekTakeGuestToken;
});

describe('Isolated shared evidence page', () => {
	it('denies an invalid fragment without ever resuming an existing guest cookie', async () => {
		handoff({ kind: 'invalid' });
		const fetchMock = vi.fn(async (_url: string, _init?: RequestInit) => Response.json(ready));
		vi.stubGlobal('fetch', fetchMock);
		await render(Share);
		await expect
			.element(page.getByRole('heading', { name: 'This link is unavailable.' }))
			.toBeVisible();
		expect(fetchMock).not.toHaveBeenCalled();
		expect(document.querySelector('video')).toBeNull();
		expect(window.__keeppeekTakeGuestToken).toBeUndefined();
	});

	it('uses only the bound guest media and hides a forbidden download', async () => {
		handoff({ kind: 'token', token: 'A'.repeat(43) });
		const fetchMock = vi.fn(async (_url: string, _init?: RequestInit) => Response.json(ready));
		vi.stubGlobal('fetch', fetchMock);
		await render(Share);
		await expect.element(page.getByRole('heading', { name: 'Front step visit' })).toBeVisible();
		expect(document.querySelector('video')?.getAttribute('src')).toBe(
			`/guest/media?session=${session}`
		);
		expect(document.querySelector('nav')).toBeNull();
		await expect.element(page.getByRole('link', { name: 'Download MP4' })).not.toBeInTheDocument();
		expect(fetchMock.mock.calls.every(([url]) => url === '/guest/session')).toBe(true);
	});

	it('renders permitted download-only evidence without opening a stream', async () => {
		handoff({ kind: 'none' });
		vi.stubGlobal(
			'fetch',
			vi.fn(async (_url: string, _init?: RequestInit) =>
				Response.json({ ...ready, can_stream: false, can_download: true })
			)
		);
		await render(Share);
		await expect
			.element(page.getByRole('link', { name: 'Download MP4' }))
			.toHaveAttribute('href', `/guest/download?session=${session}`);
		expect(document.querySelector('video')).toBeNull();
	});

	it('tears down media when polling discovers another tab changed the cookie session', async () => {
		vi.useFakeTimers();
		handoff({ kind: 'none' });
		let requests = 0;
		vi.stubGlobal(
			'fetch',
			vi.fn(async (_url: string, _init?: RequestInit) =>
				Response.json(
					++requests === 1
						? ready
						: {
								...ready,
								session_id: otherSession,
								display_name: 'Other evidence'
							}
				)
			)
		);
		await render(Share);
		await vi.waitFor(() => expect(document.querySelector('video')).not.toBeNull());
		await vi.advanceTimersByTimeAsync(1_000);
		vi.useRealTimers();
		await expect
			.element(page.getByRole('heading', { name: 'This link is unavailable.' }))
			.toBeVisible();
		expect(document.querySelector('video')).toBeNull();
		await expect.element(page.getByText('Other evidence')).not.toBeInTheDocument();
	});

	it('rejects a late exchange after Close instead of reopening the review', async () => {
		handoff({ kind: 'token', token: 'A'.repeat(43) });
		const pending = Promise.withResolvers<Response>();
		vi.stubGlobal(
			'fetch',
			vi.fn(() => pending.promise)
		);
		await render(Share);
		await page.getByRole('button', { name: 'Close review', exact: true }).click();
		pending.resolve(Response.json(ready));
		await pending.promise;
		await tick();
		await expect.element(page.getByRole('heading', { name: 'Review closed.' })).toBeVisible();
		expect(document.querySelector('video')).toBeNull();
	});

	it('clears old media for a later invalid fragment without falling back to its cookie', async () => {
		handoff({ kind: 'none' });
		const fetchMock = vi.fn(async (_url: string, _init?: RequestInit) => Response.json(ready));
		vi.stubGlobal('fetch', fetchMock);
		await render(Share);
		await expect.element(page.getByRole('heading', { name: 'Front step visit' })).toBeVisible();
		handoff({ kind: 'invalid' });
		window.dispatchEvent(new Event('keeppeek-guest-handoff'));
		await expect
			.element(page.getByRole('heading', { name: 'This link is unavailable.' }))
			.toBeVisible();
		expect(document.querySelector('video')).toBeNull();
		expect(fetchMock).toHaveBeenCalledOnce();
	});
});
