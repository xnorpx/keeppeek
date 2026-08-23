import { describe, expect, it } from 'vitest';
import {
	demandScore,
	GridStreamScheduler,
	type GridTileDemand,
	webDecoderBudget
} from './grid-stream-scheduler';

function demand(cameraId: string, options: Partial<GridTileDemand> = {}): GridTileDemand {
	return {
		cameraId,
		visibleFraction: 0,
		distanceFromViewportPx: Number.POSITIVE_INFINITY,
		viewportExtentPx: 800,
		focused: false,
		fullscreen: false,
		selectedForAudio: false,
		screenActive: true,
		mode: 'live',
		...options
	};
}

describe('GridStreamScheduler', () => {
	it('scores focus, visibility, prefetch, audio, and release grace deterministically', () => {
		expect(
			demandScore(
				demand('front', {
					focused: true,
					visibleFraction: 0.5,
					selectedForAudio: true
				}),
				true
			)
		).toBe(1_750);
		expect(demandScore(demand('near', { distanceFromViewportPx: 0 }), false)).toBe(150);
	});

	it('admits at most three new cameras every forty milliseconds', () => {
		const scheduler = new GridStreamScheduler({ subscriptionSlots: 8, decoderSlots: 8 });
		const demands = Array.from({ length: 8 }, (_, index) =>
			demand(`camera-${index}`, { visibleFraction: 1 })
		);

		const first = scheduler.reconcile(demands, 1_000);
		expect(first.grants.map((grant) => grant.cameraId)).toEqual([
			'camera-0',
			'camera-1',
			'camera-2'
		]);
		expect(first.nextReconcileAtMs).toBe(1_040);

		const early = scheduler.reconcile(demands, 1_039);
		expect(early.grants).toHaveLength(3);
		const second = scheduler.reconcile(demands, 1_040);
		expect(second.grants).toHaveLength(6);
		const third = scheduler.reconcile(demands, 1_080);
		expect(third.grants).toHaveLength(8);
	});

	it('keeps an offscreen grant for one second and lets focus preempt capacity', () => {
		const scheduler = new GridStreamScheduler({ subscriptionSlots: 1, decoderSlots: 1 });
		const front = demand('front', { visibleFraction: 1 });
		expect(scheduler.reconcile([front], 0).grants[0]?.cameraId).toBe('front');

		const leaving = demand('front');
		expect(scheduler.reconcile([leaving], 999).grants[0]?.cameraId).toBe('front');
		expect(scheduler.reconcile([leaving], 1_001).grants).toHaveLength(0);

		const focused = demand('garage', { focused: true });
		expect(scheduler.reconcile([front, focused], 2_000).grants[0]?.cameraId).toBe('garage');
	});

	it('bounds the browser decoder budget by device concurrency', () => {
		expect(webDecoderBudget(undefined)).toBe(4);
		expect(webDecoderBudget(4)).toBe(4);
		expect(webDecoderBudget(16)).toBe(8);
		expect(webDecoderBudget(64)).toBe(12);
	});
});
