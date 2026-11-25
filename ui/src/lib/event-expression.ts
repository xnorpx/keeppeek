import { create } from '@bufbuild/protobuf';
import { timestampDate, timestampFromDate } from '@bufbuild/protobuf/wkt';
import type { EventBrowserFilters } from './event-browser';
import type { RecordingEvent } from './types';
import {
	EventConfidenceOperator,
	EventConfidencePredicateSchema,
	EventOrigin,
	EventOriginListPredicateSchema,
	EventSearchExpressionListSchema,
	EventSearchExpressionSchema,
	EventSearchField,
	EventSearchPredicateSchema,
	EventStringListPredicateSchema,
	EventTextPredicateSchema,
	EventTimeRangePredicateSchema,
	EventWorkflowPredicate,
	type EventSearchExpression as ProtoEventSearchExpression
} from './proto/webrtc_pb';

export const EVENT_SEARCH_ADVANCED_CAPABILITY = 'keeppeek.event-search.advanced.v1';
export const EVENT_SEARCH_SEMANTIC_CAPABILITY = 'keeppeek.event-search.semantic.v1';
export const EVENT_EXPRESSION_VERSION = 1;
export const MAX_EXPRESSION_DEPTH = 8;
export const MAX_EXPRESSION_NODES = 64;
export const MAX_EXPRESSION_VALUES = 64;
export const MAX_EXPRESSION_TERM_BYTES = 256;
export const MAX_EXPRESSION_MODEL_BYTES = 128;
export const MAX_EXPRESSION_TEXT_BYTES = 16 * 1024;
export const MAX_URL_EXPRESSION_BYTES = 2048;
const MAX_TIME_WINDOW_MS = 31 * 86_400_000;

export type EventTextField = 'event_type' | 'face_name' | 'object_class' | 'text';
export type EventWorkflowStatePredicate =
	'reviewed' | 'dismissed' | 'bookmarked' | 'bookmarked_by_me';

export type EventExpressionPredicate =
	| { kind: 'camera_ids'; values: string[] }
	| { kind: 'group_ids'; values: string[] }
	| { kind: 'event_types'; values: string[] }
	| { kind: 'origins'; values: RecordingEvent['source'][] }
	| { kind: 'zones'; values: string[] }
	| { kind: 'missing_zone' }
	| { kind: 'confidence'; operator: 'at_least' | 'at_most'; threshold: number }
	| { kind: 'missing_confidence' }
	| { kind: 'has_image' }
	| { kind: 'time_range'; startMs: number; endMs: number }
	| { kind: 'text'; field?: EventTextField; prefix: string }
	| { kind: 'workflow'; state: EventWorkflowStatePredicate }
	| { kind: 'producer_ids'; values: string[] }
	| { kind: 'model_ids'; values: string[] }
	| { kind: 'event_ids'; values: string[] };

export type EventExpressionNode =
	| { op: 'and'; children: EventExpressionNode[] }
	| { op: 'or'; children: EventExpressionNode[] }
	| { op: 'not'; child: EventExpressionNode | null }
	| { op: 'predicate'; predicate: EventExpressionPredicate };

const encoder = new TextEncoder();

type ValidationBudget = {
	nodes: number;
	values: number;
	textBytes: number;
};

/** Create a default predicate leaf for new builder rows. */
export function defaultExpressionPredicate(): EventExpressionPredicate {
	return { kind: 'event_types', values: ['person'] };
}

/** Validate a boolean expression draft without mutating or discarding invalid state. */
export function validateEventExpression(node: EventExpressionNode | null): string | null {
	if (node === null) return 'Add at least one condition.';
	const budget: ValidationBudget = {
		nodes: MAX_EXPRESSION_NODES,
		values: MAX_EXPRESSION_VALUES,
		textBytes: MAX_EXPRESSION_TEXT_BYTES
	};
	return validateNode(node, 1, budget);
}

function validateNode(
	node: EventExpressionNode,
	depth: number,
	budget: ValidationBudget
): string | null {
	if (depth > MAX_EXPRESSION_DEPTH) {
		return `Expression depth cannot exceed ${MAX_EXPRESSION_DEPTH} levels.`;
	}
	if (budget.nodes <= 0) {
		return `Expression cannot exceed ${MAX_EXPRESSION_NODES} groups and conditions.`;
	}
	budget.nodes -= 1;
	if (node.op === 'not') {
		if (node.child === null) return 'A NOT group must contain one condition or group.';
		return validateNode(node.child, depth + 1, budget);
	}
	if (node.op === 'and' || node.op === 'or') {
		if (node.children.length === 0) {
			return `An ${node.op.toUpperCase()} group must contain at least one condition.`;
		}
		let minConfidence: number | null = null;
		let maxConfidence: number | null = null;
		let minStartMs: number | null = null;
		let maxEndMs: number | null = null;
		for (const child of node.children) {
			const error = validateNode(child, depth + 1, budget);
			if (error) return error;
			if (node.op === 'and' && child.op === 'predicate') {
				if (child.predicate.kind === 'confidence') {
					if (child.predicate.operator === 'at_least') {
						minConfidence =
							minConfidence === null
								? child.predicate.threshold
								: Math.max(minConfidence, child.predicate.threshold);
					} else {
						maxConfidence =
							maxConfidence === null
								? child.predicate.threshold
								: Math.min(maxConfidence, child.predicate.threshold);
					}
				} else if (child.predicate.kind === 'time_range') {
					minStartMs =
						minStartMs === null
							? child.predicate.startMs
							: Math.max(minStartMs, child.predicate.startMs);
					maxEndMs =
						maxEndMs === null ? child.predicate.endMs : Math.min(maxEndMs, child.predicate.endMs);
				}
			}
		}
		if (minConfidence !== null && maxConfidence !== null && minConfidence > maxConfidence) {
			return 'Contradictory confidence range inside AND group.';
		}
		if (minStartMs !== null && maxEndMs !== null && minStartMs >= maxEndMs) {
			return 'Contradictory time range inside AND group.';
		}
		return null;
	}
	return validatePredicate(node.predicate, budget);
}

function validatePredicate(
	predicate: EventExpressionPredicate,
	budget: ValidationBudget
): string | null {
	switch (predicate.kind) {
		case 'camera_ids':
		case 'group_ids':
		case 'event_types':
		case 'zones':
		case 'event_ids':
		case 'producer_ids':
		case 'model_ids': {
			if (predicate.values.length === 0) return 'Enter at least one value for every list filter.';
			budget.values -= predicate.values.length;
			if (budget.values < 0) return `Filter values cannot exceed ${MAX_EXPRESSION_VALUES} total.`;
			const maxBytes =
				predicate.kind === 'producer_ids' || predicate.kind === 'model_ids'
					? MAX_EXPRESSION_MODEL_BYTES
					: MAX_EXPRESSION_TERM_BYTES;
			for (const raw of predicate.values) {
				const trimmed = raw.trim();
				if (!trimmed) return 'List filter values must not be empty.';
				const bytes = encoder.encode(trimmed).byteLength;
				if (bytes > maxBytes) return `Filter values cannot exceed ${maxBytes} UTF-8 bytes.`;
				budget.textBytes -= bytes;
				if (budget.textBytes < 0) return 'Total filter text exceeds 16 KiB.';
			}
			return null;
		}
		case 'origins': {
			if (predicate.values.length === 0) return 'Select at least one event origin.';
			budget.values -= predicate.values.length;
			if (budget.values < 0) return `Filter values cannot exceed ${MAX_EXPRESSION_VALUES} total.`;
			return null;
		}
		case 'confidence':
			if (
				!Number.isFinite(predicate.threshold) ||
				predicate.threshold < 0 ||
				predicate.threshold > 1
			) {
				return 'Confidence threshold must be between 0 and 1.';
			}
			return null;
		case 'time_range':
			if (
				!Number.isSafeInteger(predicate.startMs) ||
				!Number.isSafeInteger(predicate.endMs) ||
				predicate.endMs <= predicate.startMs ||
				predicate.endMs - predicate.startMs > MAX_TIME_WINDOW_MS
			) {
				return 'Time range must have a valid UTC start before end within 31 days.';
			}
			return null;
		case 'text': {
			const trimmed = predicate.prefix.trim();
			if (!trimmed) return 'Text filter prefix must not be empty.';
			const bytes = encoder.encode(trimmed).byteLength;
			if (bytes > MAX_EXPRESSION_TERM_BYTES) {
				return `Text prefix cannot exceed ${MAX_EXPRESSION_TERM_BYTES} UTF-8 bytes.`;
			}
			budget.textBytes -= bytes;
			if (budget.textBytes < 0) return 'Total filter text exceeds 16 KiB.';
			return null;
		}
		case 'missing_zone':
		case 'missing_confidence':
		case 'has_image':
		case 'workflow':
			return null;
	}
}

/** Produce a plain-language summary of an expression tree. */
export function summarizeEventExpression(node: EventExpressionNode | null): string {
	if (node === null) return 'All events in the active UTC window';
	return summarizeNode(node, false);
}

function summarizeNode(node: EventExpressionNode, wrap: boolean): string {
	if (node.op === 'not') {
		return node.child ? `NOT (${summarizeNode(node.child, false)})` : 'NOT (incomplete)';
	}
	if (node.op === 'and' || node.op === 'or') {
		if (node.children.length === 0) return `${node.op.toUpperCase()} (empty)`;
		const separator = node.op === 'and' ? ' AND ' : ' OR ';
		const joined = node.children.map((child) => summarizeNode(child, true)).join(separator);
		return wrap && node.children.length > 1 ? `(${joined})` : joined;
	}
	return summarizePredicate(node.predicate);
}

function summarizePredicate(predicate: EventExpressionPredicate): string {
	switch (predicate.kind) {
		case 'camera_ids':
			return `camera in [${predicate.values.join(', ')}]`;
		case 'group_ids':
			return `group in [${predicate.values.join(', ')}]`;
		case 'event_types':
			return `type in [${predicate.values.join(', ')}]`;
		case 'origins':
			return `origin in [${predicate.values.join(', ')}]`;
		case 'zones':
			return `zone in [${predicate.values.join(', ')}]`;
		case 'missing_zone':
			return 'zone is missing';
		case 'confidence':
			return `confidence ${predicate.operator === 'at_least' ? '>=' : '<='} ${predicate.threshold.toFixed(2)}`;
		case 'missing_confidence':
			return 'confidence is missing';
		case 'has_image':
			return 'has image';
		case 'time_range':
			return `UTC ${new Date(predicate.startMs).toISOString().slice(11, 16)}..${new Date(predicate.endMs).toISOString().slice(11, 16)}`;
		case 'text':
			return predicate.field
				? `${predicate.field} matches "${predicate.prefix}"`
				: `text matches "${predicate.prefix}"`;
		case 'workflow':
			return `workflow is ${predicate.state.replaceAll('_', ' ')}`;
		case 'producer_ids':
			return `producer in [${predicate.values.join(', ')}]`;
		case 'model_ids':
			return `model in [${predicate.values.join(', ')}]`;
		case 'event_ids':
			return `event ID in [${predicate.values.join(', ')}]`;
	}
}

/** Convert common Events filters into an exact equivalent boolean AST. */
export function commonFiltersToExpression(
	filters: EventBrowserFilters
): EventExpressionNode | null {
	const children: EventExpressionNode[] = [];
	if (filters.cameraId) {
		children.push({
			op: 'predicate',
			predicate: { kind: 'camera_ids', values: [filters.cameraId] }
		});
	}
	if (filters.type) {
		children.push({ op: 'predicate', predicate: { kind: 'event_types', values: [filters.type] } });
	}
	if (filters.source) {
		children.push({ op: 'predicate', predicate: { kind: 'origins', values: [filters.source] } });
	}
	if (filters.zone) {
		children.push({ op: 'predicate', predicate: { kind: 'zones', values: [filters.zone] } });
	}
	if (filters.minimumConfidence !== null) {
		children.push({
			op: 'predicate',
			predicate: { kind: 'confidence', operator: 'at_least', threshold: filters.minimumConfidence }
		});
	}
	if (filters.image === 'with') {
		children.push({ op: 'predicate', predicate: { kind: 'has_image' } });
	} else if (filters.image === 'without') {
		children.push({ op: 'not', child: { op: 'predicate', predicate: { kind: 'has_image' } } });
	}
	if (filters.query.trim()) {
		children.push({ op: 'predicate', predicate: { kind: 'text', prefix: filters.query.trim() } });
	}
	if (filters.review === 'reviewed') {
		children.push({ op: 'predicate', predicate: { kind: 'workflow', state: 'reviewed' } });
	} else if (filters.review === 'dismissed') {
		children.push({ op: 'predicate', predicate: { kind: 'workflow', state: 'dismissed' } });
	} else if (filters.review === 'unreviewed') {
		children.push({
			op: 'not',
			child: {
				op: 'or',
				children: [
					{ op: 'predicate', predicate: { kind: 'workflow', state: 'reviewed' } },
					{ op: 'predicate', predicate: { kind: 'workflow', state: 'dismissed' } }
				]
			}
		});
	}
	if (filters.bookmarks === 'bookmarked') {
		children.push({ op: 'predicate', predicate: { kind: 'workflow', state: 'bookmarked' } });
	} else if (filters.bookmarks === 'mine') {
		children.push({ op: 'predicate', predicate: { kind: 'workflow', state: 'bookmarked_by_me' } });
	}
	if (children.length === 0) return null;
	return { op: 'and', children };
}

/** Convert an AST back into common filters only when semantics can be preserved exactly. */
export function expressionToCommonFilters(
	node: EventExpressionNode | null,
	base: EventBrowserFilters
): EventBrowserFilters | null {
	const result: EventBrowserFilters = {
		date: base.date,
		startTime: base.startTime,
		endTime: base.endTime,
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
	if (node === null) return result;
	const clauses = node.op === 'and' ? node.children : [node];
	for (const clause of clauses) {
		if (clause.op === 'not') {
			if (clause.child?.op === 'predicate' && clause.child.predicate.kind === 'has_image') {
				if (result.image !== 'all') return null;
				result.image = 'without';
				continue;
			}
			if (isUnreviewedNode(clause.child)) {
				if (result.review !== 'all') return null;
				result.review = 'unreviewed';
				continue;
			}
			return null;
		}
		if (clause.op !== 'predicate') return null;
		const predicate = clause.predicate;
		switch (predicate.kind) {
			case 'camera_ids':
				if (predicate.values.length !== 1 || result.cameraId !== null) return null;
				result.cameraId = predicate.values[0]!;
				break;
			case 'event_types':
				if (predicate.values.length !== 1 || result.type !== null) return null;
				result.type = predicate.values[0]!;
				break;
			case 'origins':
				if (predicate.values.length !== 1 || result.source !== null) return null;
				result.source = predicate.values[0]!;
				break;
			case 'zones':
				if (predicate.values.length !== 1 || result.zone !== null) return null;
				result.zone = predicate.values[0]!;
				break;
			case 'confidence':
				if (predicate.operator !== 'at_least' || result.minimumConfidence !== null) return null;
				result.minimumConfidence = predicate.threshold;
				break;
			case 'has_image':
				if (result.image !== 'all') return null;
				result.image = 'with';
				break;
			case 'text':
				if (predicate.field !== undefined || result.query !== '') return null;
				result.query = predicate.prefix;
				break;
			case 'workflow':
				if (predicate.state === 'reviewed' || predicate.state === 'dismissed') {
					if (result.review !== 'all') return null;
					result.review = predicate.state;
				} else if (predicate.state === 'bookmarked') {
					if (result.bookmarks !== 'all') return null;
					result.bookmarks = 'bookmarked';
				} else if (predicate.state === 'bookmarked_by_me') {
					if (result.bookmarks !== 'all') return null;
					result.bookmarks = 'mine';
				}
				break;
			default:
				return null;
		}
	}
	return result;
}

function isUnreviewedNode(node: EventExpressionNode | null): boolean {
	if (!node || node.op !== 'or' || node.children.length !== 2) return false;
	const states = node.children
		.map((child) =>
			child.op === 'predicate' && child.predicate.kind === 'workflow' ? child.predicate.state : null
		)
		.toSorted();
	return states[0] === 'dismissed' && states[1] === 'reviewed';
}

/** Encode a bounded version 1 expression for URL state; returns null when over the URL byte cap. */
export function encodeExpressionUrlParam(node: EventExpressionNode): string | null {
	const json = JSON.stringify({ v: EVENT_EXPRESSION_VERSION, expr: node });
	const bytes = encoder.encode(json);
	if (bytes.byteLength > MAX_URL_EXPRESSION_BYTES) return null;
	let binary = '';
	for (const byte of bytes) binary += String.fromCharCode(byte);
	const encoded = `v1:${btoa(binary).replaceAll('+', '-').replaceAll('/', '_').replaceAll('=', '')}`;
	return encoded.length <= MAX_URL_EXPRESSION_BYTES ? encoded : null;
}

/** Decode a bounded version 1 expression from URL state, failing closed on malformed payloads. */
export function decodeExpressionUrlParam(raw: string | null): EventExpressionNode | null {
	if (!raw || raw.length > MAX_URL_EXPRESSION_BYTES || !raw.startsWith('v1:')) return null;
	try {
		const payload = raw.slice(3).replaceAll('-', '+').replaceAll('_', '/');
		const padded = payload + '='.repeat((4 - (payload.length % 4)) % 4);
		const binary = atob(padded);
		const bytes = new Uint8Array(binary.length);
		for (let i = 0; i < binary.length; i += 1) bytes[i] = binary.charCodeAt(i);
		const parsed = JSON.parse(new TextDecoder().decode(bytes)) as {
			v?: unknown;
			expr?: unknown;
		};
		if (parsed?.v !== EVENT_EXPRESSION_VERSION || !isExpressionNode(parsed.expr)) return null;
		if (validateEventExpression(parsed.expr) !== null) return null;
		return parsed.expr;
	} catch {
		return null;
	}
}

function isExpressionNode(value: unknown, depth = 1): value is EventExpressionNode {
	if (depth > MAX_EXPRESSION_DEPTH || typeof value !== 'object' || value === null) return false;
	const candidate = value as Record<string, unknown>;
	if (candidate.op === 'and' || candidate.op === 'or') {
		return (
			Array.isArray(candidate.children) &&
			candidate.children.every((child) => isExpressionNode(child, depth + 1))
		);
	}
	if (candidate.op === 'not') {
		return candidate.child === null || isExpressionNode(candidate.child, depth + 1);
	}
	if (
		candidate.op === 'predicate' &&
		typeof candidate.predicate === 'object' &&
		candidate.predicate !== null
	) {
		return typeof (candidate.predicate as Record<string, unknown>).kind === 'string';
	}
	return false;
}

/** Convert a validated client AST into the approved protobuf EventSearchExpression message. */
export function eventExpressionToProto(node: EventExpressionNode): ProtoEventSearchExpression {
	if (node.op === 'and') {
		return create(EventSearchExpressionSchema, {
			node: {
				case: 'andGroup',
				value: create(EventSearchExpressionListSchema, {
					children: node.children.map(eventExpressionToProto)
				})
			}
		});
	}
	if (node.op === 'or') {
		return create(EventSearchExpressionSchema, {
			node: {
				case: 'orGroup',
				value: create(EventSearchExpressionListSchema, {
					children: node.children.map(eventExpressionToProto)
				})
			}
		});
	}
	if (node.op === 'not') {
		if (!node.child) throw new Error('A NOT group must contain one condition or group.');
		return create(EventSearchExpressionSchema, {
			node: { case: 'notExpr', value: eventExpressionToProto(node.child) }
		});
	}
	return create(EventSearchExpressionSchema, {
		node: { case: 'predicate', value: predicateToProto(node.predicate) }
	});
}

function predicateToProto(predicate: EventExpressionPredicate) {
	switch (predicate.kind) {
		case 'camera_ids':
			return create(EventSearchPredicateSchema, {
				kind: {
					case: 'cameraIds',
					value: create(EventStringListPredicateSchema, { values: [...predicate.values] })
				}
			});
		case 'group_ids':
			return create(EventSearchPredicateSchema, {
				kind: {
					case: 'groupIds',
					value: create(EventStringListPredicateSchema, { values: [...predicate.values] })
				}
			});
		case 'event_types':
			return create(EventSearchPredicateSchema, {
				kind: {
					case: 'eventTypes',
					value: create(EventStringListPredicateSchema, { values: [...predicate.values] })
				}
			});
		case 'origins':
			return create(EventSearchPredicateSchema, {
				kind: {
					case: 'origins',
					value: create(EventOriginListPredicateSchema, {
						origins: predicate.values.map((origin) =>
							origin === 'keeppeek' ? EventOrigin.KEEPPEEK : EventOrigin.CAMERA
						)
					})
				}
			});
		case 'zones':
			return create(EventSearchPredicateSchema, {
				kind: {
					case: 'zones',
					value: create(EventStringListPredicateSchema, { values: [...predicate.values] })
				}
			});
		case 'missing_zone':
			return create(EventSearchPredicateSchema, { kind: { case: 'missingZone', value: true } });
		case 'confidence':
			return create(EventSearchPredicateSchema, {
				kind: {
					case: 'confidence',
					value: create(EventConfidencePredicateSchema, {
						operator:
							predicate.operator === 'at_least'
								? EventConfidenceOperator.AT_LEAST
								: EventConfidenceOperator.AT_MOST,
						threshold: predicate.threshold
					})
				}
			});
		case 'missing_confidence':
			return create(EventSearchPredicateSchema, {
				kind: { case: 'missingConfidence', value: true }
			});
		case 'has_image':
			return create(EventSearchPredicateSchema, { kind: { case: 'hasImage', value: true } });
		case 'time_range':
			return create(EventSearchPredicateSchema, {
				kind: {
					case: 'timeRange',
					value: create(EventTimeRangePredicateSchema, {
						startTime: timestampFromDate(new Date(predicate.startMs)),
						endTime: timestampFromDate(new Date(predicate.endMs))
					})
				}
			});
		case 'text':
			return create(EventSearchPredicateSchema, {
				kind: {
					case: 'text',
					value: create(EventTextPredicateSchema, {
						prefix: predicate.prefix,
						field: predicate.field ? textFieldToProto(predicate.field) : undefined
					})
				}
			});
		case 'workflow':
			return create(EventSearchPredicateSchema, {
				kind: { case: 'workflow', value: workflowToProto(predicate.state) }
			});
		case 'producer_ids':
			return create(EventSearchPredicateSchema, {
				kind: {
					case: 'producerIds',
					value: create(EventStringListPredicateSchema, { values: [...predicate.values] })
				}
			});
		case 'model_ids':
			return create(EventSearchPredicateSchema, {
				kind: {
					case: 'modelIds',
					value: create(EventStringListPredicateSchema, { values: [...predicate.values] })
				}
			});
		case 'event_ids':
			return create(EventSearchPredicateSchema, {
				kind: {
					case: 'eventIds',
					value: create(EventStringListPredicateSchema, { values: [...predicate.values] })
				}
			});
	}
}

/** Convert a protobuf EventSearchExpression back into a client AST. */
export function protoToEventExpression(
	protoExpr: ProtoEventSearchExpression | undefined
): EventExpressionNode | null {
	if (!protoExpr) return null;
	switch (protoExpr.node.case) {
		case 'andGroup': {
			const children = protoExpr.node.value.children
				.map(protoToEventExpression)
				.filter((child): child is EventExpressionNode => child !== null);
			return children.length > 0 ? { op: 'and', children } : null;
		}
		case 'orGroup': {
			const children = protoExpr.node.value.children
				.map(protoToEventExpression)
				.filter((child): child is EventExpressionNode => child !== null);
			return children.length > 0 ? { op: 'or', children } : null;
		}
		case 'notExpr':
			return { op: 'not', child: protoToEventExpression(protoExpr.node.value) };
		case 'predicate': {
			const predicate = protoToPredicate(protoExpr.node.value);
			return predicate ? { op: 'predicate', predicate } : null;
		}
		default:
			return null;
	}
}

function protoToPredicate(
	predicate: Extract<ProtoEventSearchExpression['node'], { case: 'predicate' }>['value']
): EventExpressionPredicate | null {
	switch (predicate.kind.case) {
		case 'cameraIds':
			return { kind: 'camera_ids', values: [...predicate.kind.value.values] };
		case 'groupIds':
			return { kind: 'group_ids', values: [...predicate.kind.value.values] };
		case 'eventTypes':
			return { kind: 'event_types', values: [...predicate.kind.value.values] };
		case 'origins':
			return {
				kind: 'origins',
				values: predicate.kind.value.origins.map((origin) =>
					origin === EventOrigin.KEEPPEEK ? 'keeppeek' : 'camera'
				)
			};
		case 'zones':
			return { kind: 'zones', values: [...predicate.kind.value.values] };
		case 'missingZone':
			return { kind: 'missing_zone' };
		case 'confidence':
			return {
				kind: 'confidence',
				operator:
					predicate.kind.value.operator === EventConfidenceOperator.AT_MOST
						? 'at_most'
						: 'at_least',
				threshold: predicate.kind.value.threshold
			};
		case 'missingConfidence':
			return { kind: 'missing_confidence' };
		case 'hasImage':
			return { kind: 'has_image' };
		case 'timeRange':
			if (!predicate.kind.value.startTime || !predicate.kind.value.endTime) return null;
			return {
				kind: 'time_range',
				startMs: timestampDate(predicate.kind.value.startTime).getTime(),
				endMs: timestampDate(predicate.kind.value.endTime).getTime()
			};
		case 'text':
			return {
				kind: 'text',
				field: predicate.kind.value.field
					? protoToTextField(predicate.kind.value.field)
					: undefined,
				prefix: predicate.kind.value.prefix
			};
		case 'workflow': {
			const state = protoToWorkflow(predicate.kind.value);
			return state ? { kind: 'workflow', state } : null;
		}
		case 'producerIds':
			return { kind: 'producer_ids', values: [...predicate.kind.value.values] };
		case 'modelIds':
			return { kind: 'model_ids', values: [...predicate.kind.value.values] };
		case 'eventIds':
			return { kind: 'event_ids', values: [...predicate.kind.value.values] };
		default:
			return null;
	}
}

function textFieldToProto(field: EventTextField): EventSearchField {
	switch (field) {
		case 'event_type':
			return EventSearchField.EVENT_TYPE;
		case 'face_name':
			return EventSearchField.FACE_NAME;
		case 'object_class':
			return EventSearchField.OBJECT_CLASS;
		case 'text':
			return EventSearchField.TEXT;
	}
}

function protoToTextField(field: EventSearchField): EventTextField | undefined {
	switch (field) {
		case EventSearchField.EVENT_TYPE:
			return 'event_type';
		case EventSearchField.FACE_NAME:
			return 'face_name';
		case EventSearchField.OBJECT_CLASS:
			return 'object_class';
		case EventSearchField.TEXT:
			return 'text';
		default:
			return undefined;
	}
}

function workflowToProto(state: EventWorkflowStatePredicate): EventWorkflowPredicate {
	switch (state) {
		case 'reviewed':
			return EventWorkflowPredicate.REVIEWED;
		case 'dismissed':
			return EventWorkflowPredicate.DISMISSED;
		case 'bookmarked':
			return EventWorkflowPredicate.BOOKMARKED;
		case 'bookmarked_by_me':
			return EventWorkflowPredicate.BOOKMARKED_BY_ME;
	}
}

function protoToWorkflow(value: EventWorkflowPredicate): EventWorkflowStatePredicate | null {
	switch (value) {
		case EventWorkflowPredicate.REVIEWED:
			return 'reviewed';
		case EventWorkflowPredicate.DISMISSED:
			return 'dismissed';
		case EventWorkflowPredicate.BOOKMARKED:
			return 'bookmarked';
		case EventWorkflowPredicate.BOOKMARKED_BY_ME:
			return 'bookmarked_by_me';
		default:
			return null;
	}
}
