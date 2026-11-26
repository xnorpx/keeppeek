import { create } from '@bufbuild/protobuf';
import { timestampDate } from '@bufbuild/protobuf/wkt';
import {
	EVENT_EXPRESSION_VERSION,
	eventExpressionToProto,
	protoToEventExpression,
	type EventExpressionNode
} from './event-expression';
import type { EventWorkflowFilter } from './event-workflow';
import {
	DeleteSavedEventSearchSchema,
	EventAdvancedSearchSchema,
	EventSearchCommandSchema,
	EventSearchEmbeddingSchema,
	EventSemanticRankingSchema,
	ListSavedEventSearchesSchema,
	SaveEventSearchSchema,
	type EventSearchPlannerDiagnostics as ProtoEventSearchPlannerDiagnostics,
	type EventSearchSemanticSummary as ProtoEventSearchSemanticSummary,
	type SavedEventSearchRecord as ProtoSavedEventSearchRecord
} from './proto/webrtc_pb';

export type EventSemanticSummary = {
	modelId: string;
	modelVersion: string;
	producerId: string;
	dimensions: number;
	scoreInterpretation: string;
	minimumScore?: number;
	searchedStartMs: number;
	searchedEndMs: number;
	candidateCount: number;
	candidatesTruncated: boolean;
};

export type EventPlannerDiagnostics = {
	planCategory: string;
	estimatedCandidates: number;
	scannedCandidates: number;
	wallTimeMs: number;
};

export type EventSemanticRankingOptions = {
	modelId: string;
	modelVersion?: string;
	values: readonly number[];
	minimumScore?: number;
};

export type EventAdvancedSearchOptions = {
	expression: EventExpressionNode;
	expressionVersion?: number;
	sourceId?: string;
	streamId: 'main' | 'sub';
	startMs: number;
	endMs: number;
	pageSize?: number;
	pageToken?: string;
	includePreviewKeyframes?: boolean;
	workflow?: EventWorkflowFilter;
	semantic?: EventSemanticRankingOptions;
	signal?: AbortSignal;
};

export type SavedEventSearch = {
	searchId: string;
	ownerId: string;
	revision: number;
	name: string;
	expressionVersion: number;
	expression: EventExpressionNode | null;
	unsupportedDraftJson: string;
	updatedAtMs: number;
	tombstonedReferences: string[];
	editable: boolean;
};

export type SaveEventSearchOptions = {
	searchId?: string;
	expectedRevision: number;
	name: string;
	expression: EventExpressionNode;
	expressionVersion?: number;
};

/** Encode an EventAdvancedSearch protobuf clause. */
export function buildAdvancedSearchProto(options: EventAdvancedSearchOptions) {
	return create(EventAdvancedSearchSchema, {
		expressionVersion: options.expressionVersion ?? EVENT_EXPRESSION_VERSION,
		expression: eventExpressionToProto(options.expression),
		semantic: options.semantic
			? create(EventSemanticRankingSchema, {
					embedding: create(EventSearchEmbeddingSchema, {
						modelId: options.semantic.modelId,
						modelVersion: options.semantic.modelVersion ?? '',
						values: [...options.semantic.values]
					}),
					minimumScore: options.semantic.minimumScore
				})
			: undefined,
		includePreviewKeyframes: options.includePreviewKeyframes ?? false
	});
}

/** Decode semantic provenance summary from EventSearchQueryEnd. */
export function decodeSemanticSummary(
	summary: ProtoEventSearchSemanticSummary | undefined
): EventSemanticSummary | undefined {
	if (!summary) return undefined;
	return {
		modelId: summary.modelId,
		modelVersion: summary.modelVersion,
		producerId: summary.producerId,
		dimensions: summary.dimensions,
		scoreInterpretation: summary.scoreInterpretation,
		minimumScore: summary.minimumScore,
		searchedStartMs: summary.searchedStartTime
			? timestampDate(summary.searchedStartTime).getTime()
			: 0,
		searchedEndMs: summary.searchedEndTime ? timestampDate(summary.searchedEndTime).getTime() : 0,
		candidateCount: Number(summary.candidateCount),
		candidatesTruncated: summary.candidatesTruncated
	};
}

/** Decode Administrator-only planner diagnostics from EventSearchQueryEnd. */
export function decodePlannerDiagnostics(
	diagnostics: ProtoEventSearchPlannerDiagnostics | undefined
): EventPlannerDiagnostics | undefined {
	if (!diagnostics) return undefined;
	return {
		planCategory: diagnostics.planCategory,
		estimatedCandidates: Number(diagnostics.estimatedCandidates),
		scannedCandidates: Number(diagnostics.scannedCandidates),
		wallTimeMs: Number(diagnostics.wallTimeMs)
	};
}

/** Build a ListSavedEventSearches command. */
export function buildListSavedSearchesCommand(localWorkspaceId: string) {
	return create(EventSearchCommandSchema, {
		action: {
			case: 'listSavedSearches',
			value: create(ListSavedEventSearchesSchema, { localWorkspaceId })
		}
	});
}

/** Build a SaveEventSearch command. */
export function buildSaveEventSearchCommand(
	localWorkspaceId: string,
	options: SaveEventSearchOptions
) {
	return create(EventSearchCommandSchema, {
		action: {
			case: 'saveSearch',
			value: create(SaveEventSearchSchema, {
				localWorkspaceId,
				searchId: options.searchId ?? '',
				expectedRevision: BigInt(options.expectedRevision),
				name: options.name,
				expressionVersion: options.expressionVersion ?? EVENT_EXPRESSION_VERSION,
				expression: eventExpressionToProto(options.expression)
			})
		}
	});
}

/** Build a DeleteSavedEventSearch command. */
export function buildDeleteSavedSearchCommand(
	localWorkspaceId: string,
	searchId: string,
	expectedRevision: number
) {
	return create(EventSearchCommandSchema, {
		action: {
			case: 'deleteSavedSearch',
			value: create(DeleteSavedEventSearchSchema, {
				localWorkspaceId,
				searchId,
				expectedRevision: BigInt(expectedRevision)
			})
		}
	});
}

/** Decode a protobuf SavedEventSearchRecord into client state. */
export function decodeSavedEventSearch(record: ProtoSavedEventSearchRecord): SavedEventSearch {
	return {
		searchId: record.searchId,
		ownerId: record.ownerId,
		revision: Number(record.revision),
		name: record.name,
		expressionVersion: record.expressionVersion,
		expression: protoToEventExpression(record.expression),
		unsupportedDraftJson: record.unsupportedDraftJson,
		updatedAtMs: record.updatedAt ? timestampDate(record.updatedAt).getTime() : 0,
		tombstonedReferences: [...record.tombstonedReferences],
		editable: record.editable
	};
}
