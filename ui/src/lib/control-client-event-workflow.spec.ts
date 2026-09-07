import { create } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import {
	EventWorkflowCountsSchema,
	EventWorkflowResultSchema,
	EventWorkflowStateSchema,
	EventWorkflowTargetSchema,
	type Request
} from './proto/webrtc_pb';
import { EventWorkflowControlClient, eventWorkflowCounts } from './control-client-event-workflow';
import { localWorkflowId } from './event-workflow';

describe('event workflow client', () => {
	it('returns the server bookmark-library count and continuation token', async () => {
		const client = new EventWorkflowControlClient(
			async (command) => {
				expect(command.case).toBe('eventWorkflowCommand');
				if (
					command.case !== 'eventWorkflowCommand' ||
					command.value.action.case !== 'listBookmarks'
				)
					throw new Error('Expected bookmark list');
				expect(command.value.action.value.sourceIds).toEqual(['source-1']);
				expect(command.value.action.value.pageToken).toBe('page-current');
				return {
					case: 'eventWorkflowResult',
					value: create(EventWorkflowResultSchema, {
						actorId: 'alice',
						bookmarksIncluded: true,
						total: 27n,
						nextPageToken: 'page-next'
					})
				};
			},
			() => ({ actorId: 'alice', localWorkspaceId: '', administrator: false })
		);
		const result = await client.list({
			sourceIds: ['source-1'],
			startMs: 0,
			endMs: 86_400_000,
			pageToken: 'page-current'
		});
		expect(result.total).toBe(27);
		expect(result.nextPageToken).toBe('page-next');
	});

	it('preserves uint64 CAS revisions and sends only explicit event targets', async () => {
		const requests: Request['command'][] = [];
		const client = new EventWorkflowControlClient(
			async (command) => {
				requests.push(command);
				return {
					case: 'eventWorkflowResult',
					value: create(EventWorkflowResultSchema, {
						actorId: 'reviewer',
						states: [
							create(EventWorkflowStateSchema, {
								target: create(EventWorkflowTargetSchema, {
									sourceId: 'camera-1',
									eventId: 'event-1'
								}),
								reviewed: true,
								reviewRevision: 9007199254740994n,
								eventPresent: true,
								sourceAvailable: true
							})
						]
					})
				};
			},
			() => ({ actorId: 'reviewer', localWorkspaceId: '', administrator: false })
		);
		const result = await client.review([
			{
				sourceId: 'camera-1',
				eventId: 'event-1',
				expectedRevision: '9007199254740993',
				reviewed: true,
				dismissed: false
			}
		]);
		expect(result.states[0]?.reviewRevision).toBe('9007199254740994');
		const command = requests[0];
		if (command?.case !== 'eventWorkflowCommand' || command.value.action.case !== 'review') {
			throw new Error('Expected review command');
		}
		expect(command.value.localWorkspaceId).toBe('');
		expect(command.value.expectedActorId).toBe('reviewer');
		expect(command.value.action.value.changes).toHaveLength(1);
		expect(command.value.action.value.changes[0]?.expectedRevision).toBe(9007199254740993n);
		expect(command.value.action.value.changes[0]?.target?.eventId).toBe('event-1');
	});

	it('decodes authoritative counts without substituting the page length', () => {
		expect(
			eventWorkflowCounts(
				create(EventWorkflowCountsSchema, {
					total: 400n,
					unreviewed: 350n,
					reviewed: 40n,
					dismissed: 10n,
					bookmarked: 12n,
					bookmarkedByMe: 3n
				})
			)
		).toEqual({
			total: 400,
			unreviewed: 350,
			reviewed: 40,
			dismissed: 10,
			bookmarked: 12,
			bookmarkedByMe: 3
		});
	});

	it('reuses a durable local workspace identity and reports storage failure', () => {
		const values = new Map<string, string>();
		const storage = {
			getItem: (key: string) => values.get(key) ?? null,
			setItem: (key: string, value: string) => {
				values.set(key, value);
			}
		};
		const first = localWorkflowId(storage, () => '00000000-0000-4000-8000-000000000121');
		expect(localWorkflowId(storage, () => '00000000-0000-4000-8000-000000000122')).toBe(first);
		expect(() =>
			localWorkflowId(
				{
					...storage,
					getItem: () => {
						throw new Error('blocked');
					}
				},
				() => ''
			)
		).toThrow(/workspace identity/);
	});
});
