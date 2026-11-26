import { isPlaybackRate } from './playback-preferences';

export const maximumReviewCameras = 4;
export const gridDriftSlewThresholdMs = 100;
export const gridDriftSeekThresholdMs = 250;
export const gridDriftSlewFactor = 0.03;

export type GridPlaybackMode = 'live' | 'scrub' | 'playback' | 'paused';

export type GridReplayDemand = {
	cameraId: string;
	visibleFraction: number;
	focused: boolean;
	hasRecording: boolean;
};

export type GridReplaySchedule = {
	activeCameraIds: readonly string[];
	queuedCameraIds: readonly string[];
};

export class GridPlaybackSession {
	mode: GridPlaybackMode = 'live';
	selectedEpochMs = 0;
	playbackRate = 1;
	focusedSourceId: string | null = null;
	activeReplaySources: readonly string[] = [];
	#anchorMonotonicMs = 0;

	update(options: {
		mode: GridPlaybackMode;
		selectedEpochMs: number;
		playbackRate: number;
		focusedSourceId?: string | null;
		monotonicMs?: number;
	}): void {
		const monotonicMs = options.monotonicMs ?? performance.now();
		validTime(options.selectedEpochMs);
		validTime(monotonicMs);
		if (!isPlaybackRate(options.playbackRate)) throw new RangeError('Invalid playback rate');
		this.mode = options.mode;
		this.selectedEpochMs = options.selectedEpochMs;
		this.playbackRate = options.playbackRate;
		this.focusedSourceId = options.focusedSourceId ?? null;
		this.#anchorMonotonicMs = monotonicMs;
	}

	positionAt(monotonicMs: number): number {
		validTime(monotonicMs);
		return this.mode === 'playback'
			? expectedGridEpochMs(
					this.selectedEpochMs,
					this.#anchorMonotonicMs,
					monotonicMs,
					this.playbackRate
				)
			: this.selectedEpochMs;
	}

	seek(epochMs: number, monotonicMs: number): void {
		validTime(epochMs);
		validTime(monotonicMs);
		this.selectedEpochMs = epochMs;
		this.#anchorMonotonicMs = monotonicMs;
	}

	setPlaying(playing: boolean, monotonicMs: number): void {
		if (this.mode !== 'playback' && this.mode !== 'paused') return;
		const mode = playing ? 'playback' : 'paused';
		if (mode === this.mode) return;
		this.seek(this.positionAt(monotonicMs), monotonicMs);
		this.mode = mode;
	}

	setRate(rate: number, monotonicMs: number): void {
		if (!isPlaybackRate(rate)) throw new RangeError('Invalid playback rate');
		this.seek(this.positionAt(monotonicMs), monotonicMs);
		this.playbackRate = rate;
	}

	reconcile(demands: readonly GridReplayDemand[], decoderBudget: number): GridReplaySchedule {
		if (this.mode === 'live' || this.mode === 'scrub') {
			this.activeReplaySources = [];
			return {
				activeCameraIds: [],
				queuedCameraIds: [
					...new Set(
						demands.filter((demand) => demand.hasRecording).map((demand) => demand.cameraId)
					)
				]
			};
		}
		const candidates = demands
			.filter((demand) => demand.hasRecording && (demand.visibleFraction > 0 || demand.focused))
			.toSorted(
				(left, right) =>
					replayDemandScore(right) - replayDemandScore(left) ||
					left.cameraId.localeCompare(right.cameraId)
			);
		const budget = Number.isFinite(decoderBudget)
			? Math.min(maximumReviewCameras, Math.max(0, Math.floor(decoderBudget)))
			: 0;
		const selected = [...new Set(candidates.map((demand) => demand.cameraId))];
		const activeCameraIds = selected.slice(0, budget);
		this.activeReplaySources = activeCameraIds;
		return {
			activeCameraIds,
			queuedCameraIds: selected.slice(budget)
		};
	}
}

function validTime(time: number): void {
	if (!Number.isFinite(time) || time < 0) throw new RangeError('Invalid playback time');
}

export function expectedGridEpochMs(
	originEpochMs: number,
	originMonotonicMs: number,
	nowMonotonicMs: number,
	playbackRate: number
): number {
	return originEpochMs + Math.max(0, nowMonotonicMs - originMonotonicMs) * playbackRate;
}

export function needsGridDriftCorrection(driftMs: number): boolean {
	return Math.abs(driftMs) > gridDriftSeekThresholdMs;
}

export function gridPlaybackRateForDrift(baseRate: number, driftMs: number): number {
	if (!Number.isFinite(baseRate) || baseRate <= 0 || !Number.isFinite(driftMs)) return baseRate;
	if (Math.abs(driftMs) <= gridDriftSlewThresholdMs || needsGridDriftCorrection(driftMs)) {
		return baseRate;
	}
	return driftMs > 0 ? baseRate * (1 + gridDriftSlewFactor) : baseRate * (1 - gridDriftSlewFactor);
}

function replayDemandScore(demand: GridReplayDemand): number {
	if (demand.focused) return 1_000;
	if (demand.visibleFraction >= 1 / 3) return 600;
	return demand.visibleFraction > 0 ? 350 : 0;
}
