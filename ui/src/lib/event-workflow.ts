export const EVENT_WORKFLOW_CAPABILITY = 'keeppeek.event-workflow.v1';
export const EVENT_WORKFLOW_BATCH_LIMIT = 128;
export const EVENT_WORKFLOW_NOTE_BYTES = 1_024;
const WORKSPACE_STORAGE_KEY = 'keeppeek.event-workflow.workspace.v1';
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export type EventWorkflowTarget = { sourceId: string; eventId: string };
export type EventReviewFilter = 'all' | 'unreviewed' | 'reviewed' | 'dismissed';
export type EventWorkflowFilter = {
	review?: EventReviewFilter;
	bookmarked?: boolean;
	bookmarkedByMe?: boolean;
};
export type EventReviewChange = EventWorkflowTarget & {
	expectedRevision: string;
	reviewed: boolean;
	dismissed: boolean;
};
export type EventBookmarkChange = EventWorkflowTarget & {
	expectedRevision: string;
	active: boolean;
	note: string;
};
export type EventBookmark = {
	active: boolean;
	note: string;
	revision: string;
	createdBy: string;
	createdAtMs: number;
	updatedBy: string;
	updatedAtMs: number;
	eventStartMs: number;
	eventKind: string;
	audit: { revision: string; actorId: string; occurredAtMs: number; action: string }[];
};
export type EventWorkflowState = EventWorkflowTarget & {
	reviewed: boolean;
	dismissed: boolean;
	reviewRevision: string;
	reviewedAtMs: number | null;
	dismissedAtMs: number | null;
	updatedAtMs: number | null;
	bookmark: EventBookmark | null;
	eventPresent: boolean;
	mediaAvailable: boolean;
	sourceAvailable: boolean;
};
export type EventWorkflowCounts = {
	total: number;
	unreviewed: number;
	reviewed: number;
	dismissed: number;
	bookmarked: number;
	bookmarkedByMe: number;
};
export type EventWorkflowResult = {
	states: EventWorkflowState[];
	actorId: string;
	localWorkspace: boolean;
	bookmarksIncluded: boolean;
};
export type EventBookmarkQuery = {
	sourceIds?: readonly string[];
	startMs: number;
	endMs: number;
	byMe?: boolean;
	pageToken?: string;
};
export type EventBookmarkPage = EventWorkflowResult & { total: number; nextPageToken: string };
export type EventWorkflowIdentity = {
	actorId: string;
	localWorkspaceId: string;
	administrator: boolean;
};

export function eventWorkflowKey(target: EventWorkflowTarget): string {
	return `${encodeURIComponent(target.sourceId)}:${encodeURIComponent(target.eventId)}`;
}

export function localWorkflowId(
	storage: Pick<Storage, 'getItem' | 'setItem'>,
	generate: () => string = () => crypto.randomUUID()
): string {
	try {
		const stored = storage.getItem(WORKSPACE_STORAGE_KEY);
		const identity = stored ?? generate();
		if (!UUID.test(identity) || identity === '00000000-0000-0000-0000-000000000000') {
			throw new Error('Invalid workspace UUID');
		}
		if (stored === null) storage.setItem(WORKSPACE_STORAGE_KEY, identity);
		if (storage.getItem(WORKSPACE_STORAGE_KEY) !== identity)
			throw new Error('Storage did not persist');
		return identity.toLowerCase();
	} catch {
		throw new Error(
			'Your local workspace identity could not be saved. Allow site storage and retry.'
		);
	}
}
