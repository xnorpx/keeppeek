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

describe('pre-recording policy updates', () => {
	it('preserves zero and supports inheritance for all policy scopes', () => {
		const draft = {
			...emptyPolicyPatchDraft(),
			event_pre_recording_duration_secs_operation: 'set' as const,
			event_pre_recording_duration_secs: '0',
			event_recording_stream_operation: 'clear' as const
		};
		expect(policyPatchDraftDirty(draft)).toBe(true);
		const expected = {
			event_pre_recording_duration_secs: { operation: 'set', value: 0 },
			event_recording_stream: { operation: 'clear' }
		};
		expect(cameraPolicyPatch(draft)).toEqual(expected);
		expect(defaultPolicyPatch(draft)).toEqual(expected);
	});
	it.each(['', '-1', '31', '0.5', '1e1'])(
		'rejects invalid pre-recording seconds %s before sending',
		(seconds) => {
			expect(() =>
				cameraPolicyPatch({
					...emptyPolicyPatchDraft(),
					event_pre_recording_duration_secs_operation: 'set',
					event_pre_recording_duration_secs: seconds
				})
			).toThrow('Pre-recording duration');
		}
	);
});
