import { describe, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import { render } from 'vitest-browser-svelte';
import { EventWorkflow } from '../event-workflow.svelte';
import type { EventWorkflowState } from '../event-workflow';
import EventWorkflowControls from './EventWorkflowControls.svelte';

const initial: EventWorkflowState = {
	sourceId: 'camera-1',
	eventId: 'event-1',
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

describe('event workflow controls', () => {
	it('exposes accessible pressed state and preserves a failed bookmark note draft', async () => {
		const value = {
			...initial,
			bookmark: {
				active: true,
				note: 'Original note',
				revision: '1',
				createdBy: 'alice',
				createdAtMs: 1000,
				updatedBy: 'alice',
				updatedAtMs: 1000,
				eventStartMs: 1000,
				eventKind: 'motion',
				audit: []
			}
		};
		const workflow = new EventWorkflow({
			reviewEvents: async () => ({
				states: [{ ...value, reviewed: true, reviewRevision: '1' }],
				actorId: 'alice',
				localWorkspace: false,
				bookmarksIncluded: true
			}),
			bookmarkEvent: async () => {
				throw new Error('Bookmark revision conflict');
			},
			getEventWorkflow: async () => ({
				states: [value],
				actorId: 'alice',
				localWorkspace: false,
				bookmarksIncluded: true
			})
		});
		workflow.setActor('alice');
		workflow.hydrate([value]);
		render(EventWorkflowControls, {
			value,
			workflow,
			identity: { actorId: 'alice', localWorkspaceId: '', administrator: false },
			detail: true
		});
		await page.getByRole('button', { name: 'Mark event reviewed', exact: true }).click();
		await expect
			.element(page.getByRole('button', { name: 'Mark event unreviewed', exact: true }))
			.toHaveAttribute('aria-pressed', 'true');
		await page.getByRole('button', { name: 'Edit bookmark note', exact: true }).click();
		await page.getByRole('textbox', { name: 'Bookmark note' }).fill('Keep this draft');
		await page.getByRole('button', { name: 'Save bookmark note', exact: true }).click();
		await expect
			.element(page.getByRole('textbox', { name: 'Bookmark note' }))
			.toHaveValue('Keep this draft');
		expect(workflow.stateFor(value)?.bookmark?.note).toBe('Original note');
		expect(workflow.error).toMatch(/revision conflict/);
	});
});
