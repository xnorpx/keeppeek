import { describe, expect, it } from 'vitest';
import type { CameraListItem, RecordingEvent } from './types';
import {
	EVENT_BROWSER_INITIAL_WINDOW_MS,
	eventBrowserDayBounds,
	eventBrowserQueryBounds,
	eventBrowserRecordKey,
	eventBrowserSearchParams,
	eventFilterSummary,
	eventNoResultsSuggestion,
	filterEventBrowserRecords,
	parseEventBrowserFilters,
	previousEventBrowserWindow,
	type EventBrowserRecord
} from './event-browser';

const camera: CameraListItem = {
	id: 'front-door',
	ip: '192.0.2.1',
	name: 'Front Door',
	manufacturer: null,
	model: null,
	firmware_version: null,
	is_reolink: false,
	profiles: []
};

function record(
	id: string,
	kind: string,
	confidence: number | null,
	thumbnailUrl: string | null,
	startTimeMs: number
): EventBrowserRecord {
	const event: RecordingEvent = {
		id,
		source: 'camera',
		kind,
		start_time_ms: startTimeMs,
		end_time_ms: null,
		confidence,
		bbox: null,
		zone: 'porch',
		thumbnail_url: thumbnailUrl
	};
	return { camera, event };
}

describe('Events browser contract', () => {
	it('starts with a recent window and expands backward without crossing the day', () => {
		const { startMs, endMs } = eventBrowserDayBounds(
			'2026-08-18',
			Date.parse('2026-08-18T12:00:00Z')
		);
		const recent = previousEventBrowserWindow(startMs, endMs, EVENT_BROWSER_INITIAL_WINDOW_MS);
		expect(recent).toEqual({
			startMs: Date.parse('2026-08-18T11:55:00Z'),
			endMs,
			nextDurationMs: 10 * 60_000
		});
		expect(previousEventBrowserWindow(startMs, startMs + 1_000, 10 * 60_000)).toEqual({
			startMs,
			endMs: startMs + 1_000,
			nextDurationMs: 20 * 60_000
		});
		expect(previousEventBrowserWindow(startMs, startMs, 10 * 60_000)).toBeNull();
	});

	it('parses known structured filters and rejects invalid values', () => {
		const filters = parseEventBrowserFilters(
			new URLSearchParams(
				'date=2026-08-18&from=06%3A15&to=12%3A30&camera=front-door&type=person&source=camera&zone=porch&confidence=0.8&image=without&q=porch'
			),
			'2026-08-19'
		);

		expect(filters).toEqual({
			date: '2026-08-18',
			startTime: '06:15',
			endTime: '12:30',
			cameraId: 'front-door',
			type: 'person',
			source: 'camera',
			zone: 'porch',
			minimumConfidence: 0.8,
			image: 'without',
			query: 'porch'
		});
		expect(
			parseEventBrowserFilters(
				new URLSearchParams('date=nope&from=18%3A00&to=06%3A00&confidence=4'),
				'2026-08-19'
			)
		).toMatchObject({
			date: '2026-08-19',
			startTime: null,
			endTime: null,
			minimumConfidence: null
		});
	});

	it('builds a bounded UTC time range within the selected day', () => {
		expect(
			eventBrowserQueryBounds(
				{ date: '2026-08-18', startTime: '06:15', endTime: '12:30' },
				Date.parse('2026-08-19T00:00:00Z')
			)
		).toEqual({
			startMs: Date.parse('2026-08-18T06:15:00Z'),
			endMs: Date.parse('2026-08-18T12:30:00Z')
		});
	});

	it('serializes filters and selected evidence without defaults', () => {
		const selected = record('person/1', 'person', 0.9, '/thumb.jpg', 1);
		const params = eventBrowserSearchParams(
			{
				date: '2026-08-18',
				startTime: '06:15',
				endTime: '12:30',
				cameraId: null,
				type: 'person',
				source: null,
				zone: 'porch',
				minimumConfidence: null,
				image: 'all',
				query: ''
			},
			selected
		);

		expect(params.toString()).toBe(
			'date=2026-08-18&from=06%3A15&to=12%3A30&type=person&zone=porch&event=person%2F1&eventCamera=front-door'
		);
	});

	it('filters all structured fields and orders newest first', () => {
		const records = [
			record('old', 'person', 0.7, null, 10),
			record('new', 'person', 0.9, '/thumb.jpg', 30),
			record('motion', 'motion', null, null, 20)
		];
		const filtered = filterEventBrowserRecords(records, {
			date: '2026-08-18',
			startTime: null,
			endTime: null,
			cameraId: 'front-door',
			type: 'person',
			source: 'camera',
			zone: 'porch',
			minimumConfidence: 0.8,
			image: 'with',
			query: 'porch'
		});

		expect(filtered.map((item) => item.event.id)).toEqual(['new']);
	});

	it('creates stable escaped keys and names active no-results clauses', () => {
		const item = record('person/1', 'person', 0.9, null, 1);

		expect(eventBrowserRecordKey(item)).toBe('front-door:person%2F1');
		expect(
			eventFilterSummary({
				date: '2026-08-18',
				startTime: null,
				endTime: null,
				cameraId: null,
				type: 'person',
				source: null,
				zone: null,
				minimumConfidence: 0.8,
				image: 'without',
				query: ''
			})
		).toBe('type person, confidence at least 0.8, without images on 2026-08-18');
	});

	it('suggests the smallest single-clause loosening with a computed result count', () => {
		const records = [
			record('person', 'person', 0.7, null, 10),
			record('motion', 'motion', null, null, 20)
		];
		expect(
			eventNoResultsSuggestion(records, {
				date: '2026-08-18',
				startTime: null,
				endTime: null,
				cameraId: null,
				type: 'person',
				source: null,
				zone: null,
				minimumConfidence: null,
				image: 'all',
				query: 'missing'
			})
		).toEqual({ label: 'Clear “missing” · 1 results', count: 1, update: { query: '' } });
	});
});
