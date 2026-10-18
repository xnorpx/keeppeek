import { describe, expect, it } from 'vitest';
import { shareExpiry, shareQuota, shareTime } from './sharing';
import { sharingFixtureNow } from './sharing.fixture';

describe('sharing form bounds', () => {
	it('caps expiry at both 24 hours and the immutable artifact horizon', () => {
		const horizon = BigInt(sharingFixtureNow + 7_200_000);
		expect(shareExpiry('2026-10-18T07:00', horizon, sharingFixtureNow)).toBe(
			BigInt(sharingFixtureNow + 3_600_000)
		);
		expect(shareExpiry('2026-10-18T07:00:00', horizon, sharingFixtureNow)).toBe(
			BigInt(sharingFixtureNow + 3_600_000)
		);
		for (const input of [
			'',
			'not-a-time',
			'2026-10-18T06:00:00',
			'2026-10-18T08:00:01',
			'2026-10-20T06:00:00'
		])
			expect(() => shareExpiry(input, horizon, sharingFixtureNow)).toThrow();
	});
	it('treats a blank limit as unlimited and rejects zero, fractions, and overflow', () => {
		expect(shareQuota('')).toBeUndefined();
		expect(shareQuota('2')).toBe(2);
		for (const input of ['0', '-1', '1.5', '4294967296', 'bad'])
			expect(() => shareQuota(input)).toThrow();
	});
	it('labels shared media times explicitly in UTC', () => {
		const formatted = shareTime(BigInt(sharingFixtureNow));
		expect(formatted).toContain('18 Oct 2026');
		expect(formatted).toContain('06:00:00');
		expect(formatted).toEndWith('UTC');
	});
});
