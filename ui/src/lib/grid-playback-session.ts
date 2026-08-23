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

	update(options: {
		mode: GridPlaybackMode;
		selectedEpochMs: number;
		playbackRate: number;
		focusedSourceId?: string | null;
	}): void {
		this.mode = options.mode;
		this.selectedEpochMs = options.selectedEpochMs;
		this.playbackRate = options.playbackRate;
		this.focusedSourceId = options.focusedSourceId ?? null;
	}

	reconcile(demands: readonly GridReplayDemand[], decoderBudget: number): GridReplaySchedule {
		if (this.mode === 'live' || this.mode === 'scrub') {
			this.activeReplaySources = [];
			return {
				activeCameraIds: [],
				queuedCameraIds: demands
					.filter((demand) => demand.hasRecording)
					.map((demand) => demand.cameraId)
			};
		}
		const candidates = demands
			.filter((demand) => demand.hasRecording && (demand.visibleFraction > 0 || demand.focused))
			.toSorted(
				(left, right) =>
					replayDemandScore(right) - replayDemandScore(left) ||
					left.cameraId.localeCompare(right.cameraId)
			);
		const activeCameraIds = candidates
			.slice(0, Math.max(0, Math.floor(decoderBudget)))
			.map((demand) => demand.cameraId);
		const active = new Set(activeCameraIds);
		this.activeReplaySources = activeCameraIds;
		return {
			activeCameraIds,
			queuedCameraIds: candidates
				.filter((demand) => !active.has(demand.cameraId))
				.map((demand) => demand.cameraId)
		};
	}
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
	return Math.abs(driftMs) > 250;
}

function replayDemandScore(demand: GridReplayDemand): number {
	if (demand.focused) return 1_000;
	if (demand.visibleFraction >= 1 / 3) return 600;
	return demand.visibleFraction > 0 ? 350 : 0;
}
