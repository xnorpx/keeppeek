import type { StoredMediaPlayback } from './control-client';
import { UnsupportedRecordedFrame, type PresentedStoredFrame } from './stored-frame-step';

type Direction = -1 | 1;
type Selection = Omit<PresentedStoredFrame, 'frame'>;
type Cursor = Pick<
	StoredMediaPlayback,
	'anchorTimeMs' | 'url' | 'canStepExactly' | 'stepRecordedFrame'
>;

export class RecordedFramePlayer {
	selection = $state.raw<Selection>();
	busy = $state(false);
	notice = $state('');
	unsupported = $state(false);
	displayedNs = $state<bigint>();
	private active?: AbortController;
	private handoff?: AbortController;
	private pendingSteps = 0;

	clear(): void {
		this.handoff?.abort();
		this.handoff = undefined;
		this.active?.abort();
		this.active = undefined;
		this.pendingSteps = 0;
		this.selection = undefined;
		this.displayedNs = undefined;
		this.busy = false;
		this.notice = '';
		this.unsupported = false;
	}

	follow(video: HTMLVideoElement, cursor: Cursor): () => void {
		this.clear();
		if (typeof video.requestVideoFrameCallback !== 'function' || !cursor.canStepExactly)
			return () => this.clear();
		const url = cursor.url;
		const current = () => video.currentSrc === url && cursor.url === url;
		const timestamp = (microseconds: number) =>
			BigInt(cursor.anchorTimeMs) * 1_000_000n + BigInt(microseconds) * 1_000n;
		let generation = 0;
		let handle: number | undefined;
		let stagedUs: number | undefined;
		const observe = () => {
			const expected = ++generation;
			stagedUs = undefined;
			if (handle !== undefined) video.cancelVideoFrameCallback(handle);
			const presented: VideoFrameRequestCallback = (_now, { mediaTime }) => {
				if (expected !== generation) return;
				if (current() && Number.isFinite(mediaTime)) {
					const us = Math.round(mediaTime * 1_000_000);
					if (video.seeking) stagedUs = us;
					else this.displayedNs = timestamp(us);
				}
				handle = video.requestVideoFrameCallback(presented);
			};
			handle = video.requestVideoFrameCallback(presented);
		};
		const seeking = () => {
			if (!this.handoff) this.clear();
			observe();
		};
		const seeked = () => {
			// A paused video may present the target before seeked and never submit another frame.
			if (current() && !video.seeking && stagedUs === Math.round(video.currentTime * 1_000_000)) {
				this.displayedNs = timestamp(stagedUs);
			}
			stagedUs = undefined;
		};
		video.addEventListener('seeking', seeking);
		video.addEventListener('seeked', seeked);
		observe();
		return () => {
			generation++;
			if (handle !== undefined) video.cancelVideoFrameCallback(handle);
			video.removeEventListener('seeking', seeking);
			video.removeEventListener('seeked', seeked);
			this.clear();
		};
	}

	async alignForPlay(video: HTMLVideoElement, cursor: Cursor): Promise<boolean> {
		const sample = this.selection;
		if (!sample || this.handoff || !video.paused || video.currentSrc !== cursor.url) return false;
		const relativeNs = sample.storedTimeNs - BigInt(cursor.anchorTimeMs) * 1_000_000n;
		const targetUs = Math.round(Number(relativeNs) / 1_000);
		const controller = new AbortController();
		this.handoff = controller;
		this.active?.abort();
		this.pendingSteps = 0;
		this.notice = '';
		try {
			const displayed = await new Promise<boolean>((resolve) => {
				const signal = controller.signal;
				let callback: number;
				let presented = false;
				const done = (success: boolean) => {
					clearTimeout(timer);
					video.cancelVideoFrameCallback(callback);
					video.removeEventListener('seeked', settled);
					signal.removeEventListener('abort', cancelled);
					resolve(success);
				};
				const settled = () => {
					if (presented && !video.seeking) done(true);
				};
				const cancelled = () => done(false);
				const inspect: VideoFrameRequestCallback = (_now, { mediaTime }) => {
					if (video.currentSrc !== cursor.url || !video.paused) return done(false);
					presented = Math.abs(Math.round(mediaTime * 1_000_000) - targetUs) <= 1;
					if (presented && !video.seeking) done(true);
					else callback = video.requestVideoFrameCallback(inspect);
				};
				const timer = setTimeout(() => done(false), 5_000);
				video.addEventListener('seeked', settled);
				signal.addEventListener('abort', cancelled, { once: true });
				callback = video.requestVideoFrameCallback(inspect);
				// Enter the target sample after the browser's microsecond time conversion.
				video.currentTime = (targetUs + 1) / 1_000_000;
			});
			if (controller.signal.aborted || this.selection !== sample) return false;
			if (!displayed) {
				this.notice =
					'Playback could not resume on this recorded picture. Select another playback position.';
				return false;
			}
			this.selection = undefined;
			return true;
		} finally {
			if (this.handoff === controller) this.handoff = undefined;
		}
	}

	async step(
		direction: Direction,
		cursor: Cursor,
		present: (sample: PresentedStoredFrame) => void
	): Promise<void> {
		if (
			this.unsupported ||
			this.handoff ||
			!cursor.canStepExactly ||
			(!this.selection && this.displayedNs === undefined)
		)
			return;
		this.pendingSteps = Math.max(-32, Math.min(32, this.pendingSteps + direction));
		if (this.active) return;
		const active = new AbortController();
		this.active = active;
		this.busy = true;
		this.notice = '';
		try {
			while (this.pendingSteps !== 0) {
				const next = this.pendingSteps > 0 ? 1 : -1;
				this.pendingSteps -= next;
				const anchor = this.selection
					? { sampleToken: this.selection.sampleToken }
					: { displayedTimeNs: this.displayedNs! };
				const { frame, ...selection } = await cursor.stepRecordedFrame(next, anchor, active.signal);
				try {
					active.signal.throwIfAborted();
					present({ ...selection, frame });
					this.selection = selection;
				} finally {
					frame.close();
				}
			}
		} catch (cause) {
			if (!active.signal.aborted) {
				this.unsupported = cause instanceof UnsupportedRecordedFrame;
				this.notice =
					cause instanceof Error ? cause.message : 'The recorder could not step this picture.';
			}
		} finally {
			if (this.active === active) {
				this.pendingSteps = 0;
				this.active = undefined;
				this.busy = false;
			}
		}
	}
}
