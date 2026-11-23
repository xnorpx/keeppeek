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
	private pendingSteps = 0;

	clear(): void {
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
			this.clear();
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

	async step(
		direction: Direction,
		cursor: Cursor,
		present: (sample: PresentedStoredFrame) => void
	): Promise<void> {
		if (
			this.unsupported ||
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
