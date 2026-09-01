import { describe, expect, it } from 'vitest';
import {
	cameraPolicyPatch,
	defaultPolicyPatch,
	emptyPolicyPatchDraft,
	policyPatchDraftDirty
} from './configuration-editor';

describe('configuration policy draft', () => {
	it('preserves untouched fields and encodes inherited camera values as clear', () => {
		const draft = emptyPolicyPatchDraft();
		draft.backend_operation = 'clear';
		draft.recording_mode_operation = 'set';
		draft.recording_mode = 'main';

		expect(cameraPolicyPatch(draft)).toEqual({
			backend: { operation: 'clear' },
			recording_mode: { operation: 'set', value: 'main' }
		});
	});

	it('maps default clear to the built-in value without adding camera-only fields', () => {
		const draft = emptyPolicyPatchDraft();
		draft.transport_operation = 'clear';
		draft.onvif_port_operation = 'set';
		draft.onvif_port = '8000';

		expect(defaultPolicyPatch(draft)).toEqual({
			transport: { operation: 'clear' }
		});
	});

	it('reports only selected mutations as unsaved', () => {
		const draft = emptyPolicyPatchDraft();
		draft.backend = 'retina';
		expect(policyPatchDraftDirty(draft)).toBe(false);

		draft.backend_operation = 'set';
		expect(policyPatchDraftDirty(draft)).toBe(true);
	});
});
