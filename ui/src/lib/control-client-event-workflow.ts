import { create, fromBinary } from '@bufbuild/protobuf';
import {
	EventBookmarkChangeSchema,
	EventReviewChangeSchema,
	EventReviewFilter,
	EventWorkflowCommandSchema,
	EventWorkflowErrorCode,
	EventWorkflowErrorSchema,
	EventWorkflowQuerySchema,
	EventWorkflowTargetSchema,
	GetEventWorkflowSchema,
	ListEventBookmarksSchema,
	MutateEventReviewsSchema,
	type Error as ProtoError,
	type EventWorkflowCommand,
	type EventWorkflowCounts as ProtoCounts,
	type EventWorkflowState as ProtoState,
	type Ok,
	type Request
} from './proto/webrtc_pb';
import {
	EVENT_WORKFLOW_BATCH_LIMIT,
	EVENT_WORKFLOW_NOTE_BYTES,
	eventWorkflowKey,
	type EventBookmarkChange,
	type EventBookmarkPage,
	type EventBookmarkQuery,
	type EventReviewChange,
	type EventWorkflowCounts,
	type EventWorkflowFilter,
	type EventWorkflowIdentity,
	type EventWorkflowResult,
	type EventWorkflowState,
	type EventWorkflowTarget
} from './event-workflow';

export class EventWorkflowRequestError extends Error {
	constructor(
		message: string,
		readonly conflict: boolean,
		readonly current: EventWorkflowState | null
	) {
		super(message);
		this.name = 'EventWorkflowRequestError';
	}
}

export function decodeEventWorkflowError(error: ProtoError): EventWorkflowRequestError | null {
	const detail = error.details.find(
		(item) => item.typeUrl === 'type.googleapis.com/keeppeek.webrtc.v1.EventWorkflowError'
	);
	if (!detail) return null;
	try {
		const value = fromBinary(EventWorkflowErrorSchema, detail.value);
		return new EventWorkflowRequestError(
			error.message,
			value.code === EventWorkflowErrorCode.CONFLICT,
			value.current ? eventWorkflowState(value.current) : null
		);
	} catch {
		return new EventWorkflowRequestError(
			'The workflow response was invalid. Reload current state and retry.',
			false,
			null
		);
	}
}

export class EventWorkflowControlClient {
	constructor(
		private readonly sendRequest: (command: Request['command']) => Promise<Ok['result']>,
		private readonly identity: () => EventWorkflowIdentity
	) {}

	query(filter: EventWorkflowFilter = {}) {
		const review = {
			all: EventReviewFilter.ANY,
			unreviewed: EventReviewFilter.UNREVIEWED,
			reviewed: EventReviewFilter.REVIEWED,
			dismissed: EventReviewFilter.DISMISSED
		};
		return create(EventWorkflowQuerySchema, {
			localWorkspaceId: this.identity().localWorkspaceId,
			review: review[filter.review ?? 'all'],
			bookmarked: filter.bookmarked,
			bookmarkedByMe: filter.bookmarkedByMe ?? false
		});
	}

	get(targets: readonly EventWorkflowTarget[], includeAudit = false): Promise<EventWorkflowResult> {
		validateTargets(targets, includeAudit ? 1 : 16);
		return this.request(
			{
				case: 'get',
				value: create(GetEventWorkflowSchema, {
					targets: targets.map((target) => create(EventWorkflowTargetSchema, target)),
					includeAudit
				})
			},
			targets
		);
	}

	async list(query: EventBookmarkQuery): Promise<EventBookmarkPage> {
		const identity = this.identity();
		const sources = [...(query.sourceIds ?? [])];
		if (
			sources.length > 128 ||
			query.startMs >= query.endMs ||
			query.endMs - query.startMs > 31 * 86_400_000 ||
			(query.pageToken?.length ?? 0) > 4096
		)
			throw new Error('Bookmark query exceeds its limits.');
		const result = await this.sendRequest({
			case: 'eventWorkflowCommand',
			value: create(EventWorkflowCommandSchema, {
				localWorkspaceId: identity.localWorkspaceId,
				expectedActorId: identity.actorId,
				action: {
					case: 'listBookmarks',
					value: create(ListEventBookmarksSchema, {
						sourceIds: sources,
						startMs: BigInt(query.startMs),
						endMs: BigInt(query.endMs),
						byMe: query.byMe ?? false,
						pageSize: 16,
						pageToken: query.pageToken ?? ''
					})
				}
			})
		});
		if (
			result.case !== 'eventWorkflowResult' ||
			result.value.total === undefined ||
			result.value.states.length > 16
		)
			throw new Error('Bookmark library response is incomplete.');
		const states = result.value.states.map(eventWorkflowState);
		if (
			new Set(states.map(eventWorkflowKey)).size !== states.length ||
			states.some((item) => sources.length > 0 && !sources.includes(item.sourceId))
		)
			throw new Error('Bookmark library returned an invalid source scope.');
		return {
			states,
			total: integer(result.value.total),
			nextPageToken: result.value.nextPageToken,
			actorId: result.value.actorId,
			localWorkspace: result.value.localWorkspace,
			bookmarksIncluded: true
		};
	}

	review(changes: readonly EventReviewChange[]): Promise<EventWorkflowResult> {
		validateTargets(changes, EVENT_WORKFLOW_BATCH_LIMIT);
		return this.request(
			{
				case: 'review',
				value: create(MutateEventReviewsSchema, {
					changes: changes.map((change) =>
						create(EventReviewChangeSchema, {
							target: create(EventWorkflowTargetSchema, change),
							expectedRevision: BigInt(change.expectedRevision),
							reviewed: change.reviewed,
							dismissed: change.dismissed
						})
					)
				})
			},
			changes
		);
	}

	bookmark(change: EventBookmarkChange): Promise<EventWorkflowResult> {
		validateTargets([change], 1);
		if (new TextEncoder().encode(change.note).byteLength > EVENT_WORKFLOW_NOTE_BYTES) {
			throw new Error('Bookmark notes must fit within 1,024 UTF-8 bytes.');
		}
		return this.request(
			{
				case: 'bookmark',
				value: create(EventBookmarkChangeSchema, {
					target: create(EventWorkflowTargetSchema, change),
					expectedRevision: BigInt(change.expectedRevision),
					active: change.active,
					note: change.note
				})
			},
			[change]
		);
	}

	private async request(
		action: EventWorkflowCommand['action'],
		targets: readonly EventWorkflowTarget[]
	): Promise<EventWorkflowResult> {
		const identity = this.identity();
		const result = await this.sendRequest({
			case: 'eventWorkflowCommand',
			value: create(EventWorkflowCommandSchema, {
				localWorkspaceId: identity.localWorkspaceId,
				expectedActorId: identity.actorId,
				action
			})
		});
		if (result.case !== 'eventWorkflowResult')
			throw new Error('Unexpected event workflow response.');
		if (result.value.actorId !== identity.actorId || this.identity().actorId !== identity.actorId)
			throw new Error('Reviewer identity changed. Reload event state.');
		const states = result.value.states.map(eventWorkflowState);
		const expected = new Set(targets.map(eventWorkflowKey));
		if (
			states.length !== expected.size ||
			states.some((item) => !expected.delete(eventWorkflowKey(item)))
		) {
			throw new Error('Event workflow returned an incomplete or mismatched target set.');
		}
		return {
			states,
			actorId: result.value.actorId,
			localWorkspace: result.value.localWorkspace,
			bookmarksIncluded: result.value.bookmarksIncluded
		};
	}
}

function validateTargets(targets: readonly EventWorkflowTarget[], maximum: number): void {
	if (targets.length === 0 || targets.length > maximum)
		throw new Error(`Choose 1 to ${maximum} events.`);
	const keys = targets.map(eventWorkflowKey);
	const encoder = new TextEncoder();
	if (
		new Set(keys).size !== keys.length ||
		targets.some(
			(target) =>
				!target.sourceId ||
				!target.eventId ||
				encoder.encode(target.sourceId).length > 256 ||
				encoder.encode(target.eventId).length > 256
		)
	) {
		throw new Error('Event identities must be unique and bounded.');
	}
	if (keys.reduce((bytes, key) => bytes + encoder.encode(key).length, 0) > 16_384)
		throw new Error('Select fewer events for this action.');
}

function integer(value: bigint): number {
	const result = Number(value);
	if (!Number.isSafeInteger(result)) throw new Error('Event workflow returned an invalid number.');
	return result;
}

export function eventWorkflowState(value: ProtoState): EventWorkflowState {
	if (!value.target) throw new Error('Event workflow omitted its event identity.');
	validateTargets([value.target], 1);
	const bookmark = value.bookmark;
	if (
		bookmark &&
		(new TextEncoder().encode(bookmark.note).length > EVENT_WORKFLOW_NOTE_BYTES ||
			bookmark.audit.length > 16)
	)
		throw new Error('Event bookmark exceeds its limits.');
	return {
		sourceId: value.target.sourceId,
		eventId: value.target.eventId,
		reviewed: value.reviewed,
		dismissed: value.dismissed,
		reviewRevision: value.reviewRevision.toString(),
		reviewedAtMs: value.reviewedAtMs === undefined ? null : integer(value.reviewedAtMs),
		dismissedAtMs: value.dismissedAtMs === undefined ? null : integer(value.dismissedAtMs),
		updatedAtMs: value.updatedAtMs === undefined ? null : integer(value.updatedAtMs),
		eventPresent: value.eventPresent,
		mediaAvailable: value.mediaAvailable,
		sourceAvailable: value.sourceAvailable,
		bookmark: bookmark
			? {
					active: bookmark.active,
					note: bookmark.note,
					revision: bookmark.revision.toString(),
					createdBy: bookmark.createdBy,
					createdAtMs: integer(bookmark.createdAtMs),
					updatedBy: bookmark.updatedBy,
					updatedAtMs: integer(bookmark.updatedAtMs),
					eventStartMs: integer(bookmark.eventStartMs),
					eventKind: bookmark.eventKind,
					audit: bookmark.audit.map((item) => ({
						revision: item.revision.toString(),
						actorId: item.actorId,
						occurredAtMs: integer(item.occurredAtMs),
						action: item.action
					}))
				}
			: null
	};
}

export function eventWorkflowCounts(value: ProtoCounts): EventWorkflowCounts {
	return {
		total: integer(value.total),
		unreviewed: integer(value.unreviewed),
		reviewed: integer(value.reviewed),
		dismissed: integer(value.dismissed),
		bookmarked: integer(value.bookmarked),
		bookmarkedByMe: integer(value.bookmarkedByMe)
	};
}
