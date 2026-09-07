import { describe, expect, it } from 'vitest';
import { EventWorkflow } from './event-workflow.svelte';
import { EventWorkflowRequestError } from './control-client-event-workflow';
import type { EventReviewChange, EventWorkflowResult, EventWorkflowState } from './event-workflow';

function event(eventId: string): EventWorkflowState {
	return {
		sourceId: 'source-1',
		eventId,
		reviewed: false,
		dismissed: false,
		reviewRevision: '0',
		reviewedAtMs: null,
		dismissedAtMs: null,
		updatedAtMs: null,
		bookmark: null,
		eventPresent: true,
		mediaAvailable: true,
		sourceAvailable: true
	};
}

function result(states: EventWorkflowState[]): EventWorkflowResult {
	return { states, actorId: 'alice', localWorkspace: false, bookmarksIncluded: false };
}

describe('event workflow state', () => {
	it('retries only the requested review flag while preserving a concurrent dismissal', async () => {
		const first = event('first');
		const concurrent = { ...first, dismissed: true, reviewRevision: '1' };
		const requests: EventReviewChange[][] = [];
		const workflow = new EventWorkflow({
			reviewEvents: async (changes) => {
				requests.push([...changes]);
				if (requests.length === 1)
					throw new EventWorkflowRequestError('Conflict', true, concurrent);
				return result([
					{
						...concurrent,
						reviewed: changes[0]!.reviewed,
						dismissed: changes[0]!.dismissed,
						reviewRevision: '2'
					}
				]);
			},
			bookmarkEvent: async () => result([]),
			getEventWorkflow: async () => result([concurrent])
		});
		workflow.hydrate([first]);
		expect(await workflow.review([first], { reviewed: true }, '1 visible')).toBe(false);
		expect(await workflow.retry()).toBe(true);
		expect(requests[1]?.[0]).toMatchObject({
			reviewed: true,
			dismissed: true,
			expectedRevision: '1'
		});
		expect(workflow.stateFor(first)?.dismissed).toBe(true);
	});

	it('does not replay a prior reviewer intent after identity changes during retry reload', async () => {
		const reload = Promise.withResolvers<EventWorkflowResult>();
		const first = event('first');
		let writes = 0;
		const workflow = new EventWorkflow({
			reviewEvents: async () => {
				writes += 1;
				throw new Error('Connection lost');
			},
			bookmarkEvent: async () => result([]),
			getEventWorkflow: async () => reload.promise
		});
		workflow.setActor('alice');
		workflow.hydrate([first]);
		await workflow.review([first], { reviewed: true }, '1 selected');
		const retry = workflow.retry();
		workflow.setActor('bob');
		workflow.hydrate([first]);
		reload.resolve(result([first]));
		expect(await retry).toBe(false);
		expect(writes).toBe(1);
		expect(workflow.error).toBeNull();
	});

	it('rolls back a failed optimistic bulk action without touching unselected events', async () => {
		const response = Promise.withResolvers<EventWorkflowResult>();
		const first = event('first');
		const hidden = event('hidden');
		const workflow = new EventWorkflow({
			reviewEvents: async () => response.promise,
			bookmarkEvent: async () => result([]),
			getEventWorkflow: async () => result([first])
		});
		workflow.hydrate([first, hidden]);
		workflow.select(first, true);
		const action = workflow.review([first], { reviewed: true }, '1 selected');
		expect(workflow.stateFor(first)?.reviewed).toBe(true);
		expect(workflow.stateFor(hidden)?.reviewed).toBe(false);
		response.reject(new Error('revision conflict'));
		expect(await action).toBe(false);
		expect(workflow.stateFor(first)?.reviewed).toBe(false);
		expect(workflow.selectedStates).toHaveLength(1);
		expect(workflow.error).toMatch(/revision conflict/);
	});

	it('uses the acknowledged revision for undo and ignores older refresh snapshots', async () => {
		const requests: readonly EventReviewChange[][] = [];
		const captured = requests as EventReviewChange[][];
		const first = event('first');
		const workflow = new EventWorkflow({
			reviewEvents: async (changes) => {
				captured.push([...changes]);
				return result(
					changes.map((change) => ({
						...first,
						reviewed: change.reviewed,
						dismissed: change.dismissed,
						reviewRevision: (BigInt(change.expectedRevision) + 1n).toString()
					}))
				);
			},
			bookmarkEvent: async () => result([]),
			getEventWorkflow: async () => result([first])
		});
		workflow.hydrate([first]);
		expect(await workflow.review([first], { reviewed: true }, '1 visible')).toBe(true);
		workflow.hydrate([first]);
		expect(workflow.stateFor(first)?.reviewed).toBe(true);
		expect(await workflow.undoLast()).toBe(true);
		expect(requests[1]?.[0]?.expectedRevision).toBe('1');
		expect(workflow.stateFor(first)?.reviewed).toBe(false);
		expect(workflow.stateFor(first)?.reviewRevision).toBe('2');
	});
});
