import type {
	CameraBackend,
	CameraConfigurationPatch,
	CameraDefaultPatch,
	CameraRecordingMode,
	CameraTransport,
	ConfigurationPatchValue
} from './types';

export type PatchOperation = 'unchanged' | 'set' | 'clear';

export type PolicyPatchDraft = {
	username_operation: PatchOperation;
	username_secret_reference: string;
	password_operation: PatchOperation;
	password_secret_reference: string;
	onvif_port_operation: PatchOperation;
	onvif_port: string;
	http_port_operation: PatchOperation;
	http_port: string;
	backend_operation: PatchOperation;
	backend: CameraBackend;
	transport_operation: PatchOperation;
	transport: CameraTransport;
	record_generic_motion_events_operation: PatchOperation;
	record_generic_motion_events: boolean;
	recording_mode_operation: PatchOperation;
	recording_mode: CameraRecordingMode;
	event_recording_duration_secs_operation: PatchOperation;
	event_recording_duration_secs: string;
};

export function emptyPolicyPatchDraft(): PolicyPatchDraft {
	return {
		username_operation: 'unchanged',
		username_secret_reference: '',
		password_operation: 'unchanged',
		password_secret_reference: '',
		onvif_port_operation: 'unchanged',
		onvif_port: '',
		http_port_operation: 'unchanged',
		http_port: '',
		backend_operation: 'unchanged',
		backend: 'auto',
		transport_operation: 'unchanged',
		transport: 'tcp',
		record_generic_motion_events_operation: 'unchanged',
		record_generic_motion_events: false,
		recording_mode_operation: 'unchanged',
		recording_mode: 'event-boost',
		event_recording_duration_secs_operation: 'unchanged',
		event_recording_duration_secs: '60'
	};
}

export function policyPatchDraftDirty(draft: PolicyPatchDraft): boolean {
	return (
		draft.username_operation !== 'unchanged' ||
		draft.password_operation !== 'unchanged' ||
		draft.onvif_port_operation !== 'unchanged' ||
		draft.http_port_operation !== 'unchanged' ||
		draft.backend_operation !== 'unchanged' ||
		draft.transport_operation !== 'unchanged' ||
		draft.record_generic_motion_events_operation !== 'unchanged' ||
		draft.recording_mode_operation !== 'unchanged' ||
		draft.event_recording_duration_secs_operation !== 'unchanged'
	);
}

export function cameraPolicyPatch(draft: PolicyPatchDraft): CameraConfigurationPatch {
	return compactPatch({
		username_secret_reference: stringPatch(
			draft.username_operation,
			draft.username_secret_reference,
			'Username secret reference'
		),
		password_secret_reference: stringPatch(
			draft.password_operation,
			draft.password_secret_reference,
			'Password secret reference'
		),
		onvif_port: numberPatch(draft.onvif_port_operation, draft.onvif_port, 'ONVIF port', 1, 65_535),
		http_port: numberPatch(draft.http_port_operation, draft.http_port, 'HTTP port', 1, 65_535),
		backend: valuePatch(draft.backend_operation, draft.backend),
		transport: valuePatch(draft.transport_operation, draft.transport),
		record_generic_motion_events: valuePatch(
			draft.record_generic_motion_events_operation,
			draft.record_generic_motion_events
		),
		recording_mode: valuePatch(draft.recording_mode_operation, draft.recording_mode),
		event_recording_duration_secs: numberPatch(
			draft.event_recording_duration_secs_operation,
			draft.event_recording_duration_secs,
			'Event recording duration',
			1,
			3_600
		)
	});
}

export function defaultPolicyPatch(draft: PolicyPatchDraft): CameraDefaultPatch {
	const cameraPatch = cameraPolicyPatch(draft);
	return compactPatch({
		username_secret_reference: cameraPatch.username_secret_reference,
		password_secret_reference: cameraPatch.password_secret_reference,
		backend: cameraPatch.backend,
		transport: cameraPatch.transport,
		record_generic_motion_events: cameraPatch.record_generic_motion_events,
		recording_mode: cameraPatch.recording_mode,
		event_recording_duration_secs: cameraPatch.event_recording_duration_secs
	});
}

function valuePatch<T>(
	operation: PatchOperation,
	value: T
): ConfigurationPatchValue<T> | undefined {
	if (operation === 'unchanged') return undefined;
	if (operation === 'clear') return { operation: 'clear' };
	return { operation: 'set', value };
}

function stringPatch(
	operation: PatchOperation,
	value: string,
	label: string
): ConfigurationPatchValue<string> | undefined {
	if (operation !== 'set') return valuePatch(operation, value);
	const normalized = value.trim();
	if (normalized.length === 0) throw new Error(`${label} is required.`);
	return { operation: 'set', value: normalized };
}

function numberPatch(
	operation: PatchOperation,
	value: string,
	label: string,
	minimum: number,
	maximum: number
): ConfigurationPatchValue<number> | undefined {
	if (operation !== 'set') return valuePatch(operation, 0);
	const number = Number(value);
	if (!Number.isSafeInteger(number) || number < minimum || number > maximum) {
		throw new Error(`${label} must be a whole number between ${minimum} and ${maximum}.`);
	}
	return { operation: 'set', value: number };
}

function compactPatch<T extends object>(patch: T): T {
	return Object.fromEntries(Object.entries(patch).filter(([, value]) => value !== undefined)) as T;
}
