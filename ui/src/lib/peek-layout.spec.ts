import { describe, expect, it } from 'vitest';
import {
	addPeekLayoutCamera,
	applyPeekLayoutPreset,
	createPeekLayout,
	createPeekLayoutDraft,
	deletePeekLayout,
	duplicatePeekLayout,
	movePeekLayoutItem,
	removePeekLayoutCamera,
	renamePeekLayout,
	resizePeekLayoutItem,
	selectPeekLayout,
	setPeekLayoutActivityFocus,
	setPeekLayoutPinned,
	updatePeekLayoutAudience,
	type PeekLayoutRegistry
} from './peek-layout';

const cameraIds = ['front-door', 'driveway', 'back-yard', 'porch'];

describe('Peek layout drafts', () => {
	it('creates the authored 1 + 3 layout with a middle drop target', () => {
		const draft = createPeekLayoutDraft(cameraIds.slice(0, 3));

		expect(draft.items).toEqual([
			{
				cameraId: 'front-door',
				column: 1,
				row: 1,
				columnSpan: 8,
				rowSpan: 12,
				pinned: true
			},
			{
				cameraId: 'driveway',
				column: 9,
				row: 1,
				columnSpan: 4,
				rowSpan: 4,
				pinned: false
			},
			{
				cameraId: 'back-yard',
				column: 9,
				row: 9,
				columnSpan: 4,
				rowSpan: 4,
				pinned: false
			}
		]);
	});

	it('places an unassigned camera in the first open 4 by 4 region', () => {
		const draft = createPeekLayoutDraft(cameraIds.slice(0, 3));
		const next = addPeekLayoutCamera(draft, 'porch');

		expect(next.items.at(-1)).toMatchObject({
			cameraId: 'porch',
			column: 9,
			row: 5,
			columnSpan: 4,
			rowSpan: 4
		});
		expect(draft.items).toHaveLength(3);
	});

	it('places a camera in the largest smaller gap when no 4 by 4 region remains', () => {
		const draft = {
			preset: 'custom' as const,
			activityFocus: true,
			items: [
				{
					cameraId: 'front-door',
					column: 1,
					row: 1,
					columnSpan: 10,
					rowSpan: 12,
					pinned: true
				}
			]
		};

		const next = addPeekLayoutCamera(draft, 'driveway');

		expect(next.items.at(-1)).toMatchObject({
			cameraId: 'driveway',
			column: 11,
			row: 1,
			columnSpan: 2,
			rowSpan: 2
		});
	});

	it('removes a retained camera tile without mutating the saved draft', () => {
		const draft = createPeekLayoutDraft(cameraIds.slice(0, 3));

		const next = removePeekLayoutCamera(draft, 'back-yard');

		expect(next.preset).toBe('custom');
		expect(next.items.map((item) => item.cameraId)).toEqual(['front-door', 'driveway']);
		expect(draft.items.map((item) => item.cameraId)).toEqual([
			'front-door',
			'driveway',
			'back-yard'
		]);
	});

	it('snaps movement to the grid while rejecting collisions', () => {
		const draft = createPeekLayoutDraft(cameraIds.slice(0, 3));
		const moved = movePeekLayoutItem(draft, 'driveway', 9.2, 2.4);
		const rejected = movePeekLayoutItem(moved, 'driveway', 8, 5);

		expect(moved.items.find((item) => item.cameraId === 'driveway')).toMatchObject({
			column: 9,
			row: 2
		});
		expect(moved.preset).toBe('custom');
		expect(rejected).toBe(moved);
	});

	it('resizes on the grid while rejecting overlap with another tile', () => {
		const draft = createPeekLayoutDraft(cameraIds.slice(0, 3));
		const resized = resizePeekLayoutItem(draft, 'front-door', 7, 11);
		const rejected = resizePeekLayoutItem(resized, 'front-door', 9, 12);

		expect(resized.items.find((item) => item.cameraId === 'front-door')).toMatchObject({
			columnSpan: 7,
			rowSpan: 11
		});
		expect(resized.preset).toBe('custom');
		expect(rejected).toBe(resized);
	});

	it('applies presets without losing activity focus or explicit pins', () => {
		let draft = createPeekLayoutDraft(cameraIds.slice(0, 3));
		draft = setPeekLayoutActivityFocus(draft, false);
		draft = setPeekLayoutPinned(draft, 'driveway', true);

		const next = applyPeekLayoutPreset(draft, cameraIds, '2x2');

		expect(next.preset).toBe('2x2');
		expect(next.activityFocus).toBe(false);
		expect(next.items).toHaveLength(4);
		expect(next.items.filter((item) => item.pinned).map((item) => item.cameraId)).toEqual([
			'front-door',
			'driveway'
		]);
	});

	it('creates, renames, duplicates, selects, and deletes private layouts', () => {
		const defaultDraft = createPeekLayoutDraft(cameraIds.slice(0, 3));
		const registry: PeekLayoutRegistry = {
			schemaVersion: 1,
			revision: '7',
			activeLayoutId: 'default',
			layouts: [
				{
					id: 'default',
					name: 'Front of house',
					scope: 'shared',
					ownerId: 'server',
					audience: { everyone: true, credentialIds: [] },
					activityFocus: defaultDraft.activityFocus,
					items: defaultDraft.items
				}
			]
		};
		const nightDraft = applyPeekLayoutPreset(defaultDraft, cameraIds, '2x2');
		const created = createPeekLayout(registry, {
			id: 'night',
			name: 'Perimeter night',
			ownerId: 'alice',
			draft: nightDraft
		});
		const renamed = renamePeekLayout(created, 'night', 'After dark');
		const duplicated = duplicatePeekLayout(renamed, 'night', {
			id: 'night-copy',
			name: 'After dark copy',
			ownerId: 'alice'
		});
		const selected = selectPeekLayout(duplicated, 'night');
		const deleted = deletePeekLayout(selected, 'night');

		expect(created.activeLayoutId).toBe('night');
		expect(renamed.layouts.find((layout) => layout.id === 'night')?.name).toBe('After dark');
		expect(duplicated.layouts.find((layout) => layout.id === 'night-copy')).toMatchObject({
			scope: 'private',
			ownerId: 'alice',
			items: nightDraft.items
		});
		expect(selected.activeLayoutId).toBe('night');
		expect(deleted.activeLayoutId).toBe('default');
		expect(deleted.layouts.map((layout) => layout.id)).toEqual(['default', 'night-copy']);
	});

	it('creates a server-owned shared layout when an Administrator requests it', () => {
		const draft = createPeekLayoutDraft(cameraIds.slice(0, 2));
		const registry: PeekLayoutRegistry = {
			schemaVersion: 1,
			revision: '1',
			activeLayoutId: 'default',
			layouts: [
				{
					id: 'default',
					name: 'Default',
					scope: 'shared',
					ownerId: 'server',
					audience: { everyone: true, credentialIds: [] },
					activityFocus: true,
					items: draft.items
				}
			]
		};

		const created = createPeekLayout(registry, {
			id: 'shared-yard',
			name: 'Shared yard',
			ownerId: 'server',
			scope: 'shared',
			draft
		});

		expect(created.layouts.at(-1)).toMatchObject({
			id: 'shared-yard',
			scope: 'shared',
			ownerId: 'server'
		});
	});

	it('updates a dashboard audience with canonical credential identities', () => {
		const draft = createPeekLayoutDraft(cameraIds.slice(0, 2));
		const registry: PeekLayoutRegistry = {
			schemaVersion: 1,
			revision: '1',
			activeLayoutId: 'default',
			layouts: [
				{
					id: 'default',
					name: 'All cameras',
					scope: 'shared',
					ownerId: 'server',
					audience: { everyone: true, credentialIds: [] },
					activityFocus: true,
					items: draft.items
				}
			]
		};

		const restricted = updatePeekLayoutAudience(registry, 'default', {
			everyone: false,
			credentialIds: [' user-b ', 'user-a', 'user-b']
		});
		const everyone = updatePeekLayoutAudience(restricted, 'default', {
			everyone: true,
			credentialIds: ['user-a']
		});

		expect(restricted.layouts[0]?.audience).toEqual({
			everyone: false,
			credentialIds: ['user-a', 'user-b']
		});
		expect(everyone.layouts[0]?.audience).toEqual({ everyone: true, credentialIds: [] });
	});
});
