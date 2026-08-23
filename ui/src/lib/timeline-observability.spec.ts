import { describe, expect, it } from 'vitest';
import { emitTimelinePerformanceEvent } from './timeline-observability';

describe('emitTimelinePerformanceEvent', () => {
	it('emits one typed browser event with monotonic timing evidence', () => {
		const originalWindow = globalThis.window;
		const browserWindow = new EventTarget();
		let received: Event | null = null;
		Object.defineProperty(globalThis, 'window', { configurable: true, value: browserWindow });
		browserWindow.addEventListener('keeppeek:timeline-performance', (event) => {
			received = event;
		});
		let event;
		try {
			event = emitTimelinePerformanceEvent('TimelineFirstPage', {
				sourceId: 'front-door',
				durationMs: 42
			});
		} finally {
			Object.defineProperty(globalThis, 'window', {
				configurable: true,
				value: originalWindow
			});
		}

		expect(event).toMatchObject({
			name: 'TimelineFirstPage',
			sourceId: 'front-door',
			durationMs: 42
		});
		expect(event.atMs).toBeGreaterThanOrEqual(0);
		expect(received).not.toBeNull();
	});
});
