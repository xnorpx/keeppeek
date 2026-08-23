import type { LiveQuality } from './types';

const RELEASE_GRACE_MS = 1_000;
const ADMISSION_BATCH_SIZE = 3;
const ADMISSION_BATCH_DELAY_MS = 40;

export type GridTileMode = 'live' | 'history';

export type GridTileDemand = {
	cameraId: string;
	visibleFraction: number;
	distanceFromViewportPx: number;
	viewportExtentPx: number;
	focused: boolean;
	fullscreen: boolean;
	selectedForAudio: boolean;
	screenActive: boolean;
	mode: GridTileMode;
};

export type GridStreamGrant = {
	cameraId: string;
	quality: LiveQuality;
	score: number;
};

export type GridSchedule = {
	grants: readonly GridStreamGrant[];
	queuedCameraIds: readonly string[];
	nextReconcileAtMs: number | null;
};

export class GridStreamScheduler {
	#subscriptionSlots: number;
	#decoderSlots: number;
	#activeCameraIds = new Set<string>();
	#lastVisibleMs = new Map<string, number>();
	#nextAdmissionAtMs = 0;

	constructor(options: { subscriptionSlots: number; decoderSlots: number }) {
		this.#subscriptionSlots = positiveCapacity(options.subscriptionSlots);
		this.#decoderSlots = positiveCapacity(options.decoderSlots);
	}

	setCapacity(options: { subscriptionSlots: number; decoderSlots: number }): void {
		this.#subscriptionSlots = positiveCapacity(options.subscriptionSlots);
		this.#decoderSlots = positiveCapacity(options.decoderSlots);
	}

	reconcile(demands: readonly GridTileDemand[], nowMs: number): GridSchedule {
		const candidates = demands
			.filter((demand) => demand.screenActive && demand.mode === 'live')
			.map((demand) => {
				if (demand.visibleFraction > 0) this.#lastVisibleMs.set(demand.cameraId, nowMs);
				const lastVisibleMs = this.#lastVisibleMs.get(demand.cameraId) ?? Number.NEGATIVE_INFINITY;
				return {
					demand,
					lastVisibleMs,
					score: demandScore(demand, nowMs - lastVisibleMs <= RELEASE_GRACE_MS)
				};
			})
			.filter((candidate) => candidate.score > 0)
			.toSorted(
				(left, right) =>
					right.score - left.score ||
					right.lastVisibleMs - left.lastVisibleMs ||
					left.demand.cameraId.localeCompare(right.demand.cameraId)
			);
		const budget = Math.min(this.#subscriptionSlots, this.#decoderSlots);
		const desired = candidates.slice(0, budget);
		const desiredIds = new Set(desired.map((candidate) => candidate.demand.cameraId));
		for (const cameraId of this.#activeCameraIds) {
			if (!desiredIds.has(cameraId)) this.#activeCameraIds.delete(cameraId);
		}

		const pendingAdmissions = desired.filter(
			(candidate) => !this.#activeCameraIds.has(candidate.demand.cameraId)
		);
		if (nowMs >= this.#nextAdmissionAtMs) {
			for (const candidate of pendingAdmissions.slice(0, ADMISSION_BATCH_SIZE)) {
				this.#activeCameraIds.add(candidate.demand.cameraId);
			}
			if (pendingAdmissions.length > ADMISSION_BATCH_SIZE) {
				this.#nextAdmissionAtMs = nowMs + ADMISSION_BATCH_DELAY_MS;
			}
		}

		const grants = desired
			.filter((candidate) => this.#activeCameraIds.has(candidate.demand.cameraId))
			.map((candidate) => ({
				cameraId: candidate.demand.cameraId,
				quality:
					candidate.demand.focused || candidate.demand.fullscreen
						? ('high' as const)
						: ('low' as const),
				score: candidate.score
			}));
		const grantedIds = new Set(grants.map((grant) => grant.cameraId));
		const queuedCameraIds = desired
			.filter((candidate) => !grantedIds.has(candidate.demand.cameraId))
			.map((candidate) => candidate.demand.cameraId);
		const graceDeadline = candidates
			.filter(
				(candidate) =>
					candidate.demand.visibleFraction === 0 &&
					nowMs - candidate.lastVisibleMs <= RELEASE_GRACE_MS
			)
			.map((candidate) => candidate.lastVisibleMs + RELEASE_GRACE_MS)
			.filter((deadline) => deadline > nowMs)
			.toSorted((left, right) => left - right)[0];
		const admissionDeadline =
			queuedCameraIds.length > 0 && this.#nextAdmissionAtMs > nowMs
				? this.#nextAdmissionAtMs
				: undefined;
		const nextReconcileAtMs = Math.min(
			graceDeadline ?? Number.POSITIVE_INFINITY,
			admissionDeadline ?? Number.POSITIVE_INFINITY
		);
		return {
			grants,
			queuedCameraIds,
			nextReconcileAtMs: Number.isFinite(nextReconcileAtMs) ? nextReconcileAtMs : null
		};
	}
}

export function demandScore(demand: GridTileDemand, withinReleaseGrace: boolean): number {
	let score = 0;
	if (demand.fullscreen || demand.focused) score += 1_000;
	if (demand.visibleFraction >= 1 / 3) score += 600;
	else if (demand.visibleFraction > 0) score += 350;
	else if (demand.distanceFromViewportPx <= demand.viewportExtentPx) score += 150;
	if (demand.selectedForAudio) score += 100;
	if (withinReleaseGrace) score += 50;
	return score;
}

export function webDecoderBudget(hardwareConcurrency: number | undefined): number {
	if (!hardwareConcurrency || !Number.isFinite(hardwareConcurrency)) return 4;
	return Math.max(4, Math.min(12, Math.floor(hardwareConcurrency / 2)));
}

function positiveCapacity(value: number): number {
	if (!Number.isFinite(value)) return 1;
	return Math.max(1, Math.floor(value));
}
