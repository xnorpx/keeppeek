import { describe, expect, it } from 'vitest';
import { eventBrowserSearchParams, parseEventBrowserFilters } from './event-browser';

describe('workflow event filters', () => {
	it('round-trips independent review and bookmark predicates with the existing query', () => {
		const parsed = parseEventBrowserFilters(
			new URLSearchParams(
				'date=2026-09-06&review=unreviewed&bookmarks=mine&camera=source-1&q=door'
			),
			'2026-09-05'
		);
		expect(parsed).toMatchObject({
			review: 'unreviewed',
			bookmarks: 'mine',
			cameraId: 'source-1',
			query: 'door'
		});
		const restored = parseEventBrowserFilters(eventBrowserSearchParams(parsed), '2026-09-05');
		expect(restored).toEqual(parsed);
	});
});
