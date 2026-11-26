import { describe, expect, it } from 'vitest';
import {
	commonFiltersToExpression,
	decodeExpressionUrlParam,
	encodeExpressionUrlParam,
	eventExpressionToProto,
	expressionToCommonFilters,
	MAX_EXPRESSION_DEPTH,
	MAX_EXPRESSION_NODES,
	protoToEventExpression,
	summarizeEventExpression,
	validateEventExpression,
	type EventExpressionNode
} from './event-expression';
import type { EventBrowserFilters } from './event-browser';

const baseFilters: EventBrowserFilters = {
	date: '2026-08-18',
	startTime: '06:00',
	endTime: '08:00',
	cameraId: null,
	type: null,
	source: null,
	zone: null,
	minimumConfidence: null,
	image: 'all',
	query: '',
	review: 'all',
	bookmarks: 'all'
};

describe('event-expression validation, conversion, URL state, and protobuf round-trip', () => {
	it('rejects empty groups, dangling NOT, depth/node/value bounds, and contradictions', () => {
		expect(validateEventExpression({ op: 'and', children: [] })).toContain('at least one');
		expect(validateEventExpression({ op: 'not', child: null })).toContain('NOT');
		expect(
			validateEventExpression({
				op: 'and',
				children: [
					{
						op: 'predicate',
						predicate: { kind: 'confidence', operator: 'at_least', threshold: 0.9 }
					},
					{
						op: 'predicate',
						predicate: { kind: 'confidence', operator: 'at_most', threshold: 0.4 }
					}
				]
			})
		).toContain('Contradictory confidence');
		expect(
			validateEventExpression({
				op: 'and',
				children: [
					{ op: 'predicate', predicate: { kind: 'time_range', startMs: 1000, endMs: 2000 } },
					{ op: 'predicate', predicate: { kind: 'time_range', startMs: 3000, endMs: 4000 } }
				]
			})
		).toContain('Contradictory time');

		let deep: EventExpressionNode = { op: 'predicate', predicate: { kind: 'has_image' } };
		for (let i = 0; i < MAX_EXPRESSION_DEPTH; i += 1) deep = { op: 'not', child: deep };
		expect(validateEventExpression(deep)).toContain('depth');

		const wide: EventExpressionNode = {
			op: 'or',
			children: Array.from({ length: MAX_EXPRESSION_NODES }, () => ({
				op: 'predicate' as const,
				predicate: { kind: 'has_image' as const }
			}))
		};
		expect(validateEventExpression(wide)).toContain('64');
	});

	it('converts common filters to AST and back only when semantics are preserved exactly', () => {
		const filters: EventBrowserFilters = {
			...baseFilters,
			cameraId: 'front-door',
			type: 'person',
			source: 'keeppeek',
			zone: 'porch',
			minimumConfidence: 0.75,
			image: 'without',
			query: 'alice',
			review: 'unreviewed',
			bookmarks: 'mine'
		};
		const expr = commonFiltersToExpression(filters);
		expect(expr).not.toBeNull();
		expect(validateEventExpression(expr)).toBeNull();
		expect(expressionToCommonFilters(expr, baseFilters)).toEqual(filters);

		const nonConvertible: EventExpressionNode = {
			op: 'or',
			children: [
				{ op: 'predicate', predicate: { kind: 'event_types', values: ['person'] } },
				{ op: 'predicate', predicate: { kind: 'zones', values: ['driveway'] } }
			]
		};
		expect(expressionToCommonFilters(nonConvertible, baseFilters)).toBeNull();
	});

	it('encodes bounded v1 expressions in URL params and rejects oversized or malformed payloads', () => {
		const expr: EventExpressionNode = {
			op: 'and',
			children: [
				{ op: 'predicate', predicate: { kind: 'camera_ids', values: ['front-door'] } },
				{
					op: 'not',
					child: { op: 'predicate', predicate: { kind: 'zones', values: ['street'] } }
				}
			]
		};
		const encoded = encodeExpressionUrlParam(expr);
		expect(encoded).not.toBeNull();
		expect(encoded?.startsWith('v1:')).toBe(true);
		expect(decodeExpressionUrlParam(encoded)).toEqual(expr);
		expect(decodeExpressionUrlParam('v2:invalid')).toBeNull();
		expect(decodeExpressionUrlParam('v1:!!!not-base64!!!')).toBeNull();

		const huge: EventExpressionNode = {
			op: 'or',
			children: Array.from({ length: 12 }, (_, idx) => ({
				op: 'predicate' as const,
				predicate: { kind: 'zones' as const, values: [`zone-${idx}-${'x'.repeat(200)}`] }
			}))
		};
		expect(encodeExpressionUrlParam(huge)).toBeNull();
	});

	it('round-trips every predicate kind through protobuf and produces a plain-language summary', () => {
		const expr: EventExpressionNode = {
			op: 'and',
			children: [
				{ op: 'predicate', predicate: { kind: 'camera_ids', values: ['cam-1'] } },
				{ op: 'predicate', predicate: { kind: 'group_ids', values: ['perimeter'] } },
				{ op: 'predicate', predicate: { kind: 'event_types', values: ['person', 'vehicle'] } },
				{ op: 'predicate', predicate: { kind: 'origins', values: ['keeppeek'] } },
				{
					op: 'predicate',
					predicate: { kind: 'confidence', operator: 'at_least', threshold: 0.85 }
				},
				{ op: 'predicate', predicate: { kind: 'text', field: 'face_name', prefix: 'alice' } },
				{ op: 'predicate', predicate: { kind: 'workflow', state: 'reviewed' } },
				{ op: 'predicate', predicate: { kind: 'producer_ids', values: ['cv-prod'] } },
				{ op: 'predicate', predicate: { kind: 'model_ids', values: ['clip-v2'] } },
				{ op: 'not', child: { op: 'predicate', predicate: { kind: 'missing_zone' } } }
			]
		};
		const roundTripped = protoToEventExpression(eventExpressionToProto(expr));
		expect(roundTripped).toEqual(expr);
		const summary = summarizeEventExpression(expr);
		expect(summary).toContain('camera in [cam-1]');
		expect(summary).toContain('NOT (zone is missing)');
	});
});
