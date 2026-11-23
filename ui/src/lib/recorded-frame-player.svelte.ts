import type { StoredMediaPlayback } from './control-client';
import { UnsupportedRecordedFrame, type PresentedStoredFrame } from './stored-frame-step';

type Direction = -1 | 1;
type Selection = Omit<PresentedStoredFrame, 'frame'>;
export type ResumePicture = Pick<
	Selection,
	'storedTimeNs' | 'presentationTimeNs' | 'resumeTimesNs'
>;

type RetainedPicture = {
	picture: ResumePicture;
	signal: AbortSignal;
	verified: () => boolean;
};

type Cursor = Pick<
	StoredMediaPlayback,
	| 'storedNsForMediaUs'
	| 'mediaUsForStoredNs'
	| 'onFrameClockUpdate'
	| 'url'
	| 'canStepExactly'
	| 'stepRecordedFrame'
>;

function pausedSourceReady(video: HTMLVideoElement, cursor: Cursor, retained: RetainedPicture) {
	const { signal, picture } = retained;
	const url = cursor.url;
	const ready = () =>
		cursor.url === url &&
		video.currentSrc === url &&
		video.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA &&
		!video.seeking &&
		cursor.mediaUsForStoredNs(picture.storedTimeNs) !== undefined;
	if (signal.aborted) return Promise.resolve(false);
	if (ready()) return Promise.resolve(true);
	return new Promise<boolean>((resolve) => {
		const finish = (result: boolean) => {
			clearTimeout(timer);
			unsubscribe();
			video.removeEventListener('loadeddata', loaded);
			video.removeEventListener('canplay', loaded);
			video.removeEventListener('seeked', loaded);
			video.removeEventListener('error', failed);
			signal.removeEventListener('abort', failed);
			resolve(result);
		};
		const failed = () => finish(false);
		const loaded = () => {
			if (cursor.url !== url) failed();
			else if (ready()) finish(true);
		};
		const timer = setTimeout(failed, 10_000);
		// A replacement video's loadeddata can run before the appended fragment publishes its clock.
		const unsubscribe = cursor.onFrameClockUpdate(loaded);
		video.addEventListener('loadeddata', loaded);
		video.addEventListener('canplay', loaded);
		video.addEventListener('seeked', loaded);
		video.addEventListener('error', failed);
		signal.addEventListener('abort', failed, { once: true });
	});
}

// Reading currentTime only identifies the requested seek position. The compositor identifies
// the stored picture actually drawn, which can differ from that position for B pictures.
function probePausedPicture(
	video: HTMLVideoElement,
	url: string,
	coordinateUs: number,
	targetUs: number,
	signal: AbortSignal,
	budgetMs: number
): Promise<boolean> {
	return new Promise((resolve) => {
		let live = true;
		let callback = 0;
		let presented: number | undefined;
		const current = () =>
			!signal.aborted &&
			video.paused &&
			video.currentSrc === url &&
			Math.abs(video.currentTime * 1_000_000 - coordinateUs) <= 3;
		const finish = (success: boolean) => {
			if (!live) return;
			live = false;
			clearTimeout(timer);
			video.cancelVideoFrameCallback(callback);
			video.removeEventListener('seeked', settled);
			signal.removeEventListener('abort', cancelled);
			resolve(success);
		};
		const settled = () => {
			if (presented !== undefined && !video.seeking)
				finish(current() && Math.abs(presented - targetUs) <= 1);
		};
		const cancelled = () => finish(false);
		const inspect: VideoFrameRequestCallback = (_now, { mediaTime }) => {
			if (!live) return;
			if (!current()) return finish(false);
			presented = Math.round(mediaTime * 1_000_000);
			settled();
			if (live) callback = video.requestVideoFrameCallback(inspect);
		};
		const timer = setTimeout(cancelled, budgetMs);
		signal.addEventListener('abort', cancelled, { once: true });
		video.addEventListener('seeked', settled);
		callback = video.requestVideoFrameCallback(inspect);
		try {
			video.currentTime = coordinateUs / 1_000_000;
		} catch {
			finish(false);
		}
	});
}

// A paused seek can skip a reordered picture that the same decoder presents during playback.
// Keep the selected canvas visible and normal playback paused at the witnessed target.
function presentWhileDecoding(
	video: HTMLVideoElement,
	url: string,
	startUs: number,
	targetUs: number,
	signal: AbortSignal,
	budgetMs: number
): Promise<boolean> {
	return new Promise((resolve) => {
		let done = false;
		let frame = 0;
		const muted = video.muted;
		const rate = video.playbackRate;
		const finish = (success: boolean) => {
			if (done) return;
			done = true;
			clearTimeout(timer);
			video.removeEventListener('seeked', start);
			video.removeEventListener('error', failed);
			video.removeEventListener('ended', failed);
			signal.removeEventListener('abort', failed);
			if (frame) video.cancelVideoFrameCallback(frame);
			if (video.currentSrc === url) video.pause();
			video.muted = muted;
			video.playbackRate = rate;
			resolve(success);
		};
		const failed = () => finish(false);
		const displayed: VideoFrameRequestCallback = (_now, metadata) => {
			if (done) return;
			if (signal.aborted || video.currentSrc !== url) return failed();
			if (!video.seeking && Math.abs(Math.round(metadata.mediaTime * 1_000_000) - targetUs) <= 1) {
				finish(true);
			} else {
				frame = video.requestVideoFrameCallback(displayed);
			}
		};
		const start = () => {
			if (done || signal.aborted || video.currentSrc !== url) return failed();
			frame = video.requestVideoFrameCallback(displayed);
			void video.play().catch(failed);
		};
		const timer = setTimeout(failed, budgetMs);
		signal.addEventListener('abort', failed, { once: true });
		video.addEventListener('error', failed, { once: true });
		video.addEventListener('ended', failed, { once: true });
		video.addEventListener('seeked', start, { once: true });
		video.muted = true;
		video.playbackRate = 0.75;
		try {
			video.currentTime = startUs / 1_000_000;
		} catch {
			failed();
		}
	});
}

function waitForPausedPaint(signal: AbortSignal, deadline: number): Promise<boolean> {
	const remaining = deadline - performance.now();
	if (signal.aborted || remaining <= 0) return Promise.resolve(false);
	return new Promise<boolean>((resolve) => {
		let settled = false;
		let frame: number;
		const finish = (painted: boolean) => {
			if (settled) return;
			settled = true;
			clearTimeout(timer);
			cancelAnimationFrame(frame);
			signal.removeEventListener('abort', cancelled);
			resolve(painted);
		};
		const cancelled = () => finish(false);
		const timer = setTimeout(cancelled, remaining);
		signal.addEventListener('abort', cancelled, { once: true });
		frame = requestAnimationFrame(() => {
			if (!settled) frame = requestAnimationFrame(() => finish(true));
		});
	});
}

export class RecordedFramePlayer {
	selection = $state.raw<Selection>();
	busy = $state(false);
	notice = $state('');
	unsupported = $state(false);
	displayedNs = $state<bigint>();
	private active?: AbortController;
	private handoff = $state.raw<AbortController>();
	revealingPicture = $state(false);

	get resumingPlayback(): boolean {
		return this.handoff !== undefined;
	}
	private preroll = false;

	get preparingPlayback(): boolean {
		return this.preroll;
	}

	cancelHandoff(): void {
		this.handoff?.abort();
	}
	private pendingSteps = 0;

	clear(): void {
		this.handoff?.abort();
		this.handoff = undefined;
		this.revealingPicture = false;
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
					else this.displayedNs = cursor.storedNsForMediaUs(us);
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
				this.displayedNs = cursor.storedNsForMediaUs(stagedUs);
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

	private async findPausedPicture(
		video: HTMLVideoElement,
		cursor: Cursor,
		coordinates: readonly bigint[],
		targetUs: number,
		signal: AbortSignal,
		current: () => boolean,
		deadline: number
	): Promise<boolean> {
		const url = cursor.url;
		const positions = coordinates
			.map((coordinate) => cursor.mediaUsForStoredNs(coordinate))
			.filter(
				(us): us is number =>
					us !== undefined &&
					us >= 0 &&
					(!Number.isFinite(video.duration) || us <= video.duration * 1_000_000)
			);
		const seekDeadline = positions.length > 1 ? deadline - 2_500 : deadline;
		for (const position of positions) {
			const remaining = seekDeadline - performance.now();
			if (remaining <= 0 || !current() || !video.paused) break;
			if (
				await probePausedPicture(
					video,
					url,
					position,
					targetUs,
					signal,
					Math.min(remaining, positions.length === 1 ? 5_000 : 350)
				)
			)
				return true;
		}
		if (positions.length < 2 || !current() || !video.paused) return false;
		const startUs = Math.max(Math.min(...positions), targetUs - 1_000_000);
		const remaining = deadline - performance.now();
		if (startUs >= targetUs || remaining <= 0) return false;
		this.preroll = true;
		try {
			return await presentWhileDecoding(video, url, startUs, targetUs, signal, remaining);
		} finally {
			this.preroll = false;
		}
	}

	async alignForPlay(
		video: HTMLVideoElement,
		cursor: Cursor,
		retained?: RetainedPicture
	): Promise<boolean> {
		const sample = retained?.picture ?? this.selection;
		const url = cursor.url;
		if (retained && !(await pausedSourceReady(video, cursor, retained))) return false;
		const ownsPicture = () => (retained ? !retained.signal.aborted : this.selection === sample);
		if (
			!sample ||
			!ownsPicture() ||
			this.handoff ||
			!video.paused ||
			video.currentSrc !== url ||
			cursor.url !== url
		)
			return false;
		const targetUs = cursor.mediaUsForStoredNs(sample.storedTimeNs);
		if (targetUs === undefined) return false;
		const coordinates = sample.resumeTimesNs ?? [sample.storedTimeNs + 1_000n];
		const controller = new AbortController();
		this.handoff = controller;
		const cancel = () => controller.abort();
		retained?.signal.addEventListener('abort', cancel, { once: true });
		this.active?.abort();
		this.pendingSteps = 0;
		this.notice = '';
		const current = () =>
			!controller.signal.aborted && ownsPicture() && cursor.url === url && video.currentSrc === url;
		const deadline = performance.now() + 5_000;
		try {
			const displayed = await this.findPausedPicture(
				video,
				cursor,
				coordinates,
				targetUs,
				controller.signal,
				current,
				deadline
			);
			if (displayed && current() && video.paused) {
				// Keep ownership of the canvas until the verified video has had a chance to paint.
				this.revealingPicture = true;
				const painted = await waitForPausedPaint(controller.signal, deadline);
				if (painted && current() && video.paused && (!retained || retained.verified())) {
					this.displayedNs = sample.storedTimeNs;
					this.selection = undefined;
					return true;
				}
			}
			if (!controller.signal.aborted && ownsPicture())
				this.notice =
					'Playback could not resume on this recorded picture. Select another playback position.';
			return false;
		} finally {
			retained?.signal.removeEventListener('abort', cancel);
			if (this.handoff === controller) {
				this.revealingPicture = false;
				this.handoff = undefined;
			}
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
