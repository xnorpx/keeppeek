import { describe, expect, it } from 'vitest';
import {
	keepMomentSearchParams,
	parseKeepRoute,
	recordingMomentUrl,
	safeEventReturnHref
} from './keep-link';

const timestampMs = Date.parse('2026-09-05T23:59:59.987Z');
const moment = {
	cameraId: 'front/door + north?&',
	streamPreference: 'auto' as const,
	timestampMs,
	mode: 'timeline' as const
};

describe('authenticated recording-moment routes', () => {
	it('round-trips opaque source identity, exact milliseconds, and automatic preference', () => {
		const route = parseKeepRoute(keepMomentSearchParams(moment).toString());
		expect(route).toEqual({
			...moment,
			date: '2026-09-05',
			eventId: null,
			returnHref: null,
			invalid: false
		});
	});

	it('treats the absolute timestamp as authoritative across UTC day boundaries', () => {
		const route = parseKeepRoute(
			`camera=front&stream=sub&date=2026-09-05&at=${timestampMs + 13}&mode=stories`
		);
		expect(route).toMatchObject({
			timestampMs: timestampMs + 13,
			date: '2026-09-06',
			mode: 'stories',
			streamPreference: 'sub'
		});
	});

	it.each(['auto', 'high', 'low', 'main', 'sub'] as const)(
		'preserves %s stream preference',
		(streamPreference) => {
			expect(
				parseKeepRoute(keepMomentSearchParams({ ...moment, streamPreference }).toString())
					.streamPreference
			).toBe(streamPreference);
		}
	);

	it.each(['NaN', 'Infinity', '-1', '1e12', '1.5', '253402300800000', '999999999999999999'])(
		'rejects invalid timestamps without an unbounded date query: %s',
		(value) => {
			expect(parseKeepRoute(`camera=front&at=${value}&date=2026-02-30`)).toMatchObject({
				timestampMs: null,
				date: null,
				invalid: true
			});
		}
	);

	it('rejects duplicate and oversized identity parameters', () => {
		expect(parseKeepRoute('camera=front&camera=back&at=1').invalid).toBe(true);
		expect(parseKeepRoute(`camera=${'x'.repeat(257)}&at=1`).invalid).toBe(true);
		expect(parseKeepRoute('camera=bad%00id&at=1').invalid).toBe(true);
		expect(parseKeepRoute('x'.repeat(8193)).invalid).toBe(true);
	});
});

describe('recording-link privacy and return context', () => {
	it('builds an absolute link without copying any current query, fragment, or credentials', () => {
		const url = new URL(
			recordingMomentUrl({
				...moment,
				origin: 'https://user:private@example.net/keep?token=private#session',
				keepPath: '/nvr/keep'
			})
		);
		expect(url.origin).toBe('https://example.net');
		expect(url.pathname).toBe('/nvr/keep');
		expect(url.href).not.toContain('private');
		expect(url.hash).toBe('');
		expect([...url.searchParams.keys()]).toEqual(['camera', 'date', 'at', 'stream']);
	});

	it('preserves event context while removing unknown return parameters and fragments', () => {
		const search = keepMomentSearchParams({
			...moment,
			eventId: 'person/1',
			returnHref:
				'/events?date=2026-09-05&event=person%2F1&eventCamera=front&type=person&q=porch&token=private&session_id=secret#debug'
		});
		expect(parseKeepRoute(search.toString())).toMatchObject({
			eventId: 'person/1',
			returnHref: '/events?date=2026-09-05&type=person&q=porch&event=person%2F1&eventCamera=front'
		});
		expect(search.toString()).not.toContain('private');
		expect(search.toString()).not.toContain('secret');
	});

	it.each([
		'https://other.example/events',
		'//other.example/events',
		'/\\other.example/events',
		'/logs?token=secret',
		'/events?returnTo=/logs'
	])('rejects unsafe or unsupported return contexts: %s', (value) => {
		expect(safeEventReturnHref(value)).toBe(value === '/events?returnTo=/logs' ? '/events' : null);
	});

	it('supports the configured application base path without accepting unrelated paths', () => {
		expect(safeEventReturnHref('/nvr/events?date=2026-09-05', '/nvr/events')).toBe(
			'/nvr/events?date=2026-09-05'
		);
		expect(safeEventReturnHref('/other/events', '/nvr/events')).toBeNull();
	});

	it('requires an explicit source for timestamp or event links but preserves ordinary Keep navigation', () => {
		expect(parseKeepRoute(`at=${timestampMs}`).invalid).toBe(true);
		expect(parseKeepRoute('event=person').invalid).toBe(true);
		expect(parseKeepRoute('date=2026-09-05').invalid).toBe(false);
		expect(parseKeepRoute('').invalid).toBe(false);
	});

	it('bounds encoded return context as well as its input text', () => {
		const returnHref = `/events?q=${'\u6f22'.repeat(400)}`;
		expect(returnHref.length).toBeLessThan(2048);
		expect(safeEventReturnHref(returnHref)).toBeNull();
		const search = keepMomentSearchParams({ ...moment, returnHref });
		expect(parseKeepRoute(search.toString()).invalid).toBe(false);
		expect(search.has('returnTo')).toBe(false);
	});
});
