import { page } from 'vitest/browser';
import { describe, expect, it, vi } from 'vitest';
import { render } from 'vitest-browser-svelte';
import '../../app.css';
import Keep from './+page.svelte';

const context = vi.hoisted(() => ({ control: vi.fn() }));
vi.mock('$lib/control-context', () => ({ useControlClient: context.control }));
vi.mock('$app/navigation', () => ({ afterNavigate: vi.fn(), goto: vi.fn() }));
vi.mock('$app/paths', () => ({ resolve: (value: string) => value }));
vi.mock('$app/state', () => ({
	page: { url: new URL('http://localhost/keep'), state: {} },
	navigating: {}
}));

const start = Date.parse('2026-10-15T12:00:00Z');
const recording = {
	stream: 'sub',
	date: '2026-10-15',
	hour: '12',
	filename: 'clip.mp4',
	url: 'stored:front:sub',
	start_time_ms: start,
	end_time_ms: start + 40_000,
	duration_ms: 40_000
};

function cursor(id: string) {
	let archive = (_value: boolean) => {};
	const url = URL.createObjectURL(new MediaSource());
	const playback = {
		id,
		sourceId: 'front',
		streamId: 'sub',
		url,
		anchorTimeMs: start,
		initialOffsetSeconds: 0,
		canSeekLocally: () => false,
		seek: vi.fn(async () => archive(true)),
		setPlaybackRate: vi.fn(),
		setPlaying: vi.fn(),
		observe: vi.fn(),
		onKeyFrame: () => () => {},
		onError: () => () => {},
		onStartup: () => () => {},
		onArchive: (listener: typeof archive) => {
			archive = listener;
			listener(false);
			return () => {};
		},
		close: vi.fn(async () => {})
	};
	return {
		playback,
		emitArchive: (value: boolean) => archive(value),
		dispose: () => URL.revokeObjectURL(url)
	};
}

function controller(first: ReturnType<typeof cursor>, next: ReturnType<typeof cursor>) {
	const open = vi.fn().mockResolvedValueOnce(first.playback).mockResolvedValue(next.playback);
	context.control.mockReturnValue({
		onCapabilities: (listener: (values: string[]) => void) => {
			listener(['keeppeek.stored-media']);
			return () => {};
		},
		onAccessState: (listener: (value: { session: null }) => void) => {
			listener({ session: null });
			return () => {};
		},
		getCameras: async () => [
			{ id: 'front', name: 'Front', profiles: [{ stream: 'sub', encoding: 'H264' }] }
		],
		getHealth: async () => null,
		getRecordingDates: async () => ['2026-10-15'],
		getRecordingsInRange: async () => [{ segments: [recording], events: [] }],
		queryStoredTimeline: async () => ({ ranges: [], events: [] }),
		openStoredMedia: open
	});
	return open;
}

describe('Keep archive classification', () => {
	it('tracks the retained cursor across cold seeks and ignores a replaced cursor callback', async () => {
		await page.viewport(1280, 800);
		const original = window.location.href;
		window.history.replaceState(
			null,
			'',
			`/keep?camera=front&stream=sub&date=2026-10-15&at=${start}`
		);
		const first = cursor('first');
		const next = cursor('replacement');
		const open = controller(first, next);
		try {
			await render(Keep);
			const slider = page.getByRole('slider', { name: 'Recording position', exact: true });
			await expect.element(slider).toBeEnabled();
			await slider.click();
			await vi.waitFor(() => expect(first.playback.seek).toHaveBeenCalledOnce());
			await expect
				.element(page.getByRole('region', { name: 'Archive playback', exact: true }))
				.toBeVisible();
			await page.getByRole('button', { name: 'Close playback', exact: true }).click();
			await vi.waitFor(() => expect(first.playback.close).toHaveBeenCalledOnce());
			await page.getByRole('button', { name: 'Refresh recordings', exact: true }).click();
			await vi.waitFor(() => expect(open).toHaveBeenCalledTimes(2));
			await expect
				.element(page.getByRole('region', { name: 'Playback', exact: true }))
				.toBeVisible();
			first.emitArchive(true);
			await expect
				.element(page.getByRole('region', { name: 'Archive playback', exact: true }))
				.not.toBeInTheDocument();
		} finally {
			window.history.replaceState(null, '', original);
			first.dispose();
			next.dispose();
		}
	});
});
