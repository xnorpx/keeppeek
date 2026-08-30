import { describe, expect, it } from 'vitest';
import type { PeekLayoutRegistry } from './peek-layout';
import {
	applyPeekLayoutImport,
	exportPeekLayoutRegistry,
	previewPeekLayoutImport
} from './peek-layout-exchange';

const current: PeekLayoutRegistry = {
	schemaVersion: 1,
	revision: '9',
	activeLayoutId: 'default',
	layouts: [
		{
			id: 'default',
			name: 'Front of house',
			scope: 'shared',
			ownerId: 'server',
			audience: { everyone: true, credentialIds: [] },
			activityFocus: true,
			items: [
				{
					cameraId: 'front-door',
					column: 1,
					row: 1,
					columnSpan: 12,
					rowSpan: 12,
					pinned: true
				}
			]
		}
	]
};

describe('Peek layout exchange', () => {
	it('round-trips every supported registry field and active selection', () => {
		const source: PeekLayoutRegistry = {
			...current,
			activeLayoutId: 'night',
			layouts: [
				...current.layouts,
				{
					id: 'night',
					name: 'Perimeter night',
					scope: 'private',
					ownerId: 'alice',
					audience: { everyone: false, credentialIds: ['alice'] },
					activityFocus: false,
					items: [
						{
							cameraId: 'side-gate',
							column: 3,
							row: 2,
							columnSpan: 7,
							rowSpan: 9,
							pinned: true
						}
					]
				}
			]
		};

		const exported = exportPeekLayoutRegistry(source);
		const preview = previewPeekLayoutImport(exported, { ...current, layouts: [] }, [
			'front-door',
			'side-gate'
		]);
		const imported = applyPeekLayoutImport(
			{ ...current, layouts: [], activeLayoutId: '' },
			preview,
			{
				ownerId: 'alice',
				availableCameraIds: ['front-door', 'side-gate'],
				missingCameraMappings: {},
				conflictResolution: 'reject',
				preserveOwnership: true
			}
		);

		expect(JSON.parse(exported)).not.toHaveProperty('revision');
		expect(imported).toEqual({ ...source, revision: current.revision });
	});

	it('requires every missing camera to be mapped or omitted before applying atomically', () => {
		const source: PeekLayoutRegistry = {
			...current,
			activeLayoutId: 'night',
			layouts: [
				{
					id: 'night',
					name: 'Night',
					scope: 'private',
					ownerId: 'old-user',
					audience: { everyone: false, credentialIds: ['old-user'] },
					activityFocus: true,
					items: [{ ...current.layouts[0]!.items[0]!, cameraId: 'old-side-gate' }]
				}
			]
		};
		const preview = previewPeekLayoutImport(exportPeekLayoutRegistry(source), current, [
			'front-door',
			'side-gate'
		]);

		expect(preview.missingCameraIds).toEqual(['old-side-gate']);
		expect(() =>
			applyPeekLayoutImport(current, preview, {
				ownerId: 'alice',
				availableCameraIds: ['front-door', 'side-gate'],
				missingCameraMappings: {},
				conflictResolution: 'reject'
			})
		).toThrow('Choose a mapping or omit every missing camera.');
		expect(current.layouts).toHaveLength(1);

		const imported = applyPeekLayoutImport(current, preview, {
			ownerId: 'alice',
			availableCameraIds: ['front-door', 'side-gate'],
			missingCameraMappings: { 'old-side-gate': 'side-gate' },
			conflictResolution: 'reject'
		});
		expect(imported.layouts.at(-1)).toMatchObject({
			id: 'night',
			scope: 'private',
			ownerId: 'alice',
			items: [{ cameraId: 'side-gate' }]
		});
	});

	it('reassigns imported private ownership and protects shared conflicts', () => {
		const document = JSON.stringify({
			schema_version: 1,
			active_layout_id: 'default',
			layouts: [
				{
					id: 'default',
					name: 'Imported private default',
					scope: 'private',
					owner_id: 'old-principal',
					activity_focus: true,
					tiles: []
				}
			]
		});
		const preview = previewPeekLayoutImport(document, current, ['front-door']);

		expect(() =>
			applyPeekLayoutImport(current, preview, {
				ownerId: 'alice',
				availableCameraIds: ['front-door'],
				missingCameraMappings: {},
				conflictResolution: 'replace',
				preserveOwnership: true
			})
		).toThrow('A private layout cannot replace a shared layout.');

		const imported = applyPeekLayoutImport(current, preview, {
			ownerId: 'alice',
			availableCameraIds: ['front-door'],
			missingCameraMappings: {},
			conflictResolution: 'duplicate',
			preserveOwnership: true,
			idFactory: () => 'imported-copy'
		});
		expect(imported.layouts.at(-1)).toMatchObject({
			id: 'imported-copy',
			scope: 'private',
			ownerId: 'alice'
		});
	});

	it('reports unsupported fields and conflicts without changing the current registry', () => {
		const document = JSON.stringify({
			schema_version: 1,
			active_layout_id: 'default',
			access_key: 'must-not-be-imported',
			layouts: [
				{
					id: 'default',
					name: 'Imported default',
					scope: 'private',
					owner_id: 'alice',
					activity_focus: true,
					tiles: []
				}
			]
		});
		const preview = previewPeekLayoutImport(document, current, ['front-door']);

		expect(preview.unsupportedFields).toEqual(['access_key']);
		expect(preview.conflictingLayoutIds).toEqual(['default']);
		expect(() =>
			applyPeekLayoutImport(current, preview, {
				ownerId: 'alice',
				availableCameraIds: ['front-door'],
				missingCameraMappings: {},
				conflictResolution: 'replace'
			})
		).toThrow('Remove unsupported fields before importing.');
		expect(current.layouts[0]?.name).toBe('Front of house');
	});

	it('rejects oversized, unsupported, excessive, and duplicate-camera imports', () => {
		expect(() =>
			previewPeekLayoutImport('x'.repeat(256 * 1_024 + 1), current, ['front-door'])
		).toThrow('Layout import is too large.');
		expect(() =>
			previewPeekLayoutImport(
				JSON.stringify({ schema_version: 2, active_layout_id: 'default', layouts: [] }),
				current,
				['front-door']
			)
		).toThrow('Layout import schema version is unsupported.');
		const layout = {
			id: 'layout',
			name: 'Layout',
			scope: 'private',
			owner_id: 'alice',
			activity_focus: true,
			tiles: []
		};
		expect(() =>
			previewPeekLayoutImport(
				JSON.stringify({
					schema_version: 1,
					active_layout_id: 'layout-0',
					layouts: Array.from({ length: 33 }, (_, index) => ({
						...layout,
						id: `layout-${index}`
					}))
				}),
				current,
				['front-door']
			)
		).toThrow('Layout import has an invalid layout count.');
		expect(() =>
			previewPeekLayoutImport(
				JSON.stringify({
					schema_version: 1,
					active_layout_id: 'layout',
					layouts: [
						{
							...layout,
							tiles: [
								{
									camera_id: 'front-door',
									column: 1,
									row: 1,
									column_span: 6,
									row_span: 12,
									pinned: false
								},
								{
									camera_id: 'front-door',
									column: 7,
									row: 1,
									column_span: 6,
									row_span: 12,
									pinned: true
								}
							]
						}
					]
				}),
				current,
				['front-door']
			)
		).toThrow('Layout import contains a duplicate camera.');
		expect(() =>
			previewPeekLayoutImport(
				JSON.stringify({
					schema_version: 1,
					active_layout_id: 'valid',
					layouts: [{ ...layout, id: 'x'.repeat(129) }]
				}),
				current,
				['front-door']
			)
		).toThrow('Invalid layout ID.');
	});
});
