import { EventWorkflowRequestError } from './control-client-event-workflow';
import {
	EVENT_WORKFLOW_BATCH_LIMIT,
	eventWorkflowKey,
	type EventBookmarkChange,
	type EventReviewChange,
	type EventWorkflowResult,
	type EventWorkflowState,
	type EventWorkflowTarget
} from './event-workflow';

type Client = {
	reviewEvents(changes: readonly EventReviewChange[]): Promise<EventWorkflowResult>;
	bookmarkEvent(change: EventBookmarkChange): Promise<EventWorkflowResult>;
	getEventWorkflow(
		targets: readonly EventWorkflowTarget[],
		includeAudit?: boolean
	): Promise<EventWorkflowResult>;
};
type ReviewUndo = { before: EventWorkflowState[]; revisions: string[]; scope: string };
type ReviewPatch = Partial<Pick<EventReviewChange, 'reviewed' | 'dismissed'>>;
type RetryIntent =
	| { kind: 'review'; changes: EventReviewChange[]; scope: string; patch: ReviewPatch | null }
	| { kind: 'bookmark'; change: EventBookmarkChange };
const MAX_CACHED_STATES = 512;

export class EventWorkflow {
	values = $state.raw<Readonly<Record<string, EventWorkflowState>>>({});
	selected = $state.raw<ReadonlySet<string>>(new Set());
	pending = $state.raw<ReadonlySet<string>>(new Set());
	error = $state<string | null>(null);
	notice = $state<string | null>(null);
	undo = $state.raw<ReviewUndo | null>(null);
	retryIntent = $state.raw<RetryIntent | null>(null);
	reloading = $state(false);
	#actorId = '';
	#generation = 0;

	constructor(private readonly client: Client) {}

	get busy(): boolean {
		return this.pending.size > 0 || this.reloading;
	}
	get selectedStates(): EventWorkflowState[] {
		return [...this.selected].flatMap((key) => (this.values[key] ? [this.values[key]!] : []));
	}

	setActor(actorId: string): void {
		if (this.#actorId === actorId) return;
		this.#actorId = actorId;
		this.#generation += 1;
		this.values = {};
		this.selected = new Set();
		this.pending = new Set();
		this.reloading = false;
		this.undo = null;
		this.retryIntent = null;
		this.error = null;
		this.notice = null;
	}

	stateFor(target: EventWorkflowTarget): EventWorkflowState | null {
		return this.values[eventWorkflowKey(target)] ?? null;
	}

	hydrate(states: readonly EventWorkflowState[], bookmarksIncluded = true): void {
		const next = { ...this.values };
		for (const incoming of states) {
			const key = eventWorkflowKey(incoming);
			if (this.pending.has(key)) continue;
			const current = next[key];
			const review =
				current && BigInt(current.reviewRevision) > BigInt(incoming.reviewRevision)
					? current
					: incoming;
			const preserveBookmark =
				current &&
				(!bookmarksIncluded ||
					BigInt(current.bookmark?.revision ?? '0') > BigInt(incoming.bookmark?.revision ?? '0'));
			next[key] = {
				...incoming,
				reviewed: review.reviewed,
				dismissed: review.dismissed,
				reviewRevision: review.reviewRevision,
				reviewedAtMs: review.reviewedAtMs,
				dismissedAtMs: review.dismissedAtMs,
				updatedAtMs: review.updatedAtMs,
				bookmark: preserveBookmark ? current.bookmark : incoming.bookmark
			};
		}
		const retained = new Set([
			...states.map(eventWorkflowKey),
			...this.selected,
			...this.pending,
			...(this.undo?.before.map(eventWorkflowKey) ?? [])
		]);
		const entries = Object.entries(next);
		let excess = entries.length - MAX_CACHED_STATES;
		for (const [key] of entries) {
			if (excess <= 0) break;
			if (!retained.has(key)) {
				delete next[key];
				excess -= 1;
			}
		}
		this.values = next;
	}

	select(target: EventWorkflowState, checked: boolean): void {
		const key = eventWorkflowKey(target);
		const next = new Set(this.selected);
		if (checked) {
			if (next.size >= EVENT_WORKFLOW_BATCH_LIMIT && !next.has(key)) {
				this.error = `Select at most ${EVENT_WORKFLOW_BATCH_LIMIT} events.`;
				return;
			}
			next.add(key);
		} else next.delete(key);
		this.hydrate([target]);
		this.selected = next;
	}

	async load(
		targets: readonly EventWorkflowTarget[],
		includeAudit = false,
		signal?: AbortSignal
	): Promise<void> {
		if (targets.length > EVENT_WORKFLOW_BATCH_LIMIT || (includeAudit && targets.length !== 1))
			throw new Error('Event workflow read scope exceeds its limit.');
		const generation = this.#generation;
		for (let offset = 0; offset < targets.length; offset += 16) {
			if (signal?.aborted) return;
			const response = await this.client.getEventWorkflow(
				targets.slice(offset, offset + 16),
				includeAudit
			);
			if (generation !== this.#generation || signal?.aborted) return;
			this.hydrate(response.states, response.bookmarksIncluded);
		}
	}

	review(
		states: readonly EventWorkflowState[],
		patch: ReviewPatch,
		scope: string
	): Promise<boolean> {
		const changes = states.map((item) => ({
			sourceId: item.sourceId,
			eventId: item.eventId,
			expectedRevision: item.reviewRevision,
			reviewed: patch.reviewed ?? item.reviewed,
			dismissed: patch.dismissed ?? item.dismissed
		}));
		return this.applyReview(changes, [...states], scope, true, patch);
	}

	async undoLast(): Promise<boolean> {
		const undo = this.undo;
		if (!undo) return false;
		const changes = undo.before.map((item, index) => ({
			sourceId: item.sourceId,
			eventId: item.eventId,
			expectedRevision: undo.revisions[index]!,
			reviewed: item.reviewed,
			dismissed: item.dismissed
		}));
		const before = changes.flatMap((item) => (this.stateFor(item) ? [this.stateFor(item)!] : []));
		const changed = await this.applyReview(changes, before, undo.scope, false);
		if (changed) this.undo = null;
		return changed;
	}

	private async applyReview(
		changes: EventReviewChange[],
		before: EventWorkflowState[],
		scope: string,
		rememberUndo: boolean,
		patch: ReviewPatch | null = null
	): Promise<boolean> {
		if (this.busy || changes.length === 0) return false;
		const generation = this.#generation;
		this.pending = new Set(changes.map(eventWorkflowKey));
		this.error = null;
		const next = { ...this.values };
		for (const change of changes) {
			const key = eventWorkflowKey(change);
			const previous = next[key];
			if (previous)
				next[key] = { ...previous, reviewed: change.reviewed, dismissed: change.dismissed };
		}
		this.values = next;
		try {
			const result = await this.client.reviewEvents(changes);
			if (generation !== this.#generation) return false;
			this.pending = new Set();
			this.hydrate(result.states, result.bookmarksIncluded);
			if (rememberUndo)
				this.undo = {
					before,
					revisions: before.map((item) => this.stateFor(item)!.reviewRevision),
					scope
				};
			this.retryIntent = null;
			this.notice = `${rememberUndo ? 'Updated' : 'Undid review changes for'} ${scope} events.`;
			return true;
		} catch (cause) {
			if (generation !== this.#generation) return false;
			this.restore(before, cause);
			this.retryIntent = { kind: 'review', changes, scope, patch };
			return false;
		} finally {
			if (generation === this.#generation) this.pending = new Set();
		}
	}

	async bookmark(
		previous: EventWorkflowState,
		active: boolean,
		note: string,
		expectedRevision = previous.bookmark?.revision ?? '0'
	): Promise<boolean> {
		if (this.busy) return false;
		const generation = this.#generation;
		const change = {
			sourceId: previous.sourceId,
			eventId: previous.eventId,
			active,
			note,
			expectedRevision
		};
		this.pending = new Set([eventWorkflowKey(previous)]);
		this.error = null;
		if (previous.bookmark)
			this.values = {
				...this.values,
				[eventWorkflowKey(previous)]: {
					...previous,
					bookmark: { ...previous.bookmark, active, note }
				}
			};
		try {
			const result = await this.client.bookmarkEvent(change);
			if (generation !== this.#generation) return false;
			this.pending = new Set();
			this.hydrate(result.states, true);
			this.retryIntent = null;
			this.notice = active ? 'Bookmark saved.' : 'Bookmark removed.';
			return true;
		} catch (cause) {
			if (generation !== this.#generation) return false;
			this.restore([previous], cause);
			this.retryIntent = { kind: 'bookmark', change };
			return false;
		} finally {
			if (generation === this.#generation) this.pending = new Set();
		}
	}

	async retry(): Promise<boolean> {
		const intent = this.retryIntent;
		if (!intent || this.busy) return false;
		const generation = this.#generation;
		this.reloading = true;
		try {
			const targets = intent.kind === 'review' ? intent.changes : [intent.change];
			await this.load(targets);
			if (generation !== this.#generation) return false;
			this.reloading = false;
			if (intent.kind === 'bookmark') {
				const current = this.stateFor(intent.change);
				return current ? this.bookmark(current, intent.change.active, intent.change.note) : false;
			}
			const before = intent.changes.map((change) => this.stateFor(change)!);
			const changes = intent.changes.map((change, index) => ({
				...change,
				reviewed:
					intent.patch === null
						? change.reviewed
						: (intent.patch.reviewed ?? before[index]!.reviewed),
				dismissed:
					intent.patch === null
						? change.dismissed
						: (intent.patch.dismissed ?? before[index]!.dismissed),
				expectedRevision: before[index]!.reviewRevision
			}));
			return this.applyReview(changes, before, intent.scope, true, intent.patch);
		} catch (cause) {
			if (generation === this.#generation)
				this.error =
					cause instanceof Error
						? cause.message
						: 'Could not reload event state. Retry the action.';
			return false;
		} finally {
			if (generation === this.#generation) this.reloading = false;
		}
	}

	private restore(before: EventWorkflowState[], cause: unknown): void {
		this.pending = new Set();
		this.values = {
			...this.values,
			...Object.fromEntries(before.map((item) => [eventWorkflowKey(item), item]))
		};
		if (cause instanceof EventWorkflowRequestError && cause.current) this.hydrate([cause.current]);
		this.error = `${cause instanceof Error ? cause.message : 'Event update failed.'} Your action was not confirmed. Reload and retry.`;
	}
}
