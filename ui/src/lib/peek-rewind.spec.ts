import { describe, expect, it } from 'vitest';
import type { CameraHealth } from './types';
import {
	peekRewindAnchorMs,
	peekRewindMaximumSeconds,
	peekRewindSeconds,
	peekRewindTargetMs
} from './peek-rewind';

function health(state: CameraHealth['state'], reportAgeMs: number): CameraHealth {
	return {
		id: 'front-door',
		ip: '192.0.2.1',
		name: 'Front Door',
		manufacturer: null,
		model: null,
		firmware_version: null,
		state,
		lifecycle: null,
		last_error: null,
		configured_profiles: [],
		streams: [{ type: 'main', updated_at_ms: 1, report_age_ms: reportAgeMs }]
	};
}

describe('Peek rewind timing', () => {
	it('maps vertical tile distance into the two-minute band', () => {
		expect(peekRewindSeconds(76, 240)).toBe(38);
		expect(peekRewindSeconds(240, 240)).toBe(peekRewindMaximumSeconds);
		expect(peekRewindSeconds(400, 240)).toBe(peekRewindMaximumSeconds);
	});

	it('ignores upward movement and invalid geometry', () => {
		expect(peekRewindSeconds(-10, 240)).toBe(0);
		expect(peekRewindSeconds(10, 0)).toBe(0);
	});

	it('anchors live and degraded cameras at now', () => {
		expect(peekRewindAnchorMs(health('online', 8_000), 100_000)).toBe(100_000);
		expect(peekRewindAnchorMs(health('degraded', 8_000), 100_000)).toBe(100_000);
	});

	it('anchors stale and offline cameras at their last reported frame', () => {
		expect(peekRewindAnchorMs(health('stale', 8_000), 100_000)).toBe(92_000);
		expect(peekRewindAnchorMs(health('offline', 14_000), 100_000)).toBe(86_000);
	});

	it('builds a non-negative bounded target timestamp', () => {
		expect(peekRewindTargetMs(100_000, 38)).toBe(62_000);
		expect(peekRewindTargetMs(100_000, 500)).toBe(0);
	});
});
