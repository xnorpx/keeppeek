import type {
	CameraBackend,
	CameraCatalogCamera,
	CameraRecordingMode,
	CameraCatalogStreamHints,
	CameraStreamProbeResult,
	CameraSettingsUpdate,
	CameraTransport,
	DiscoveredCameraSettings
} from './types';

export const cameraWizardSteps = ['find', 'connect', 'streams', 'recording', 'review'] as const;
export type CameraWizardStep = (typeof cameraWizardSteps)[number];

export type CameraWizardDraft = {
	ip: string;
	displayName: string;
	username: string;
	password: string;
	defaultUsernameConfigured: boolean;
	defaultPasswordConfigured: boolean;
	onvifPort: string;
	httpPort: string;
	mainRtspUrl: string;
	subRtspUrl: string;
	backend: CameraBackend;
	transport: CameraTransport;
	recordGenericMotionEvents: boolean;
	recordingMode: CameraRecordingMode;
	eventRecordingDurationSeconds: string;
	discoveryEvidence: string | null;
};

export function emptyCameraWizardDraft(): CameraWizardDraft {
	return {
		ip: '',
		displayName: '',
		username: '',
		password: '',
		defaultUsernameConfigured: false,
		defaultPasswordConfigured: false,
		onvifPort: '',
		httpPort: '',
		mainRtspUrl: '',
		subRtspUrl: '',
		backend: 'auto',
		transport: 'tcp',
		recordGenericMotionEvents: false,
		recordingMode: 'event-boost',
		eventRecordingDurationSeconds: '60',
		discoveryEvidence: null
	};
}

export function draftFromDiscoveredCamera(camera: DiscoveredCameraSettings): CameraWizardDraft {
	const reolink = camera.brand.toLocaleLowerCase() === 'reolink';
	const draft: CameraWizardDraft = {
		...emptyCameraWizardDraft(),
		ip: camera.ip,
		displayName: camera.name ?? '',
		onvifPort: (camera.onvif_port ?? (reolink ? 8000 : null))?.toString() ?? '',
		httpPort: reolink ? '80' : '',
		backend: reolink ? 'reo-proto' : 'auto',
		discoveryEvidence: [camera.brand, camera.model, ...camera.sources]
			.filter((value): value is string => Boolean(value))
			.join(' · ')
	};
	return camera.catalog?.stream_hints
		? applyCatalogStreamHints(draft, camera.catalog.stream_hints)
		: draft;
}

export function applyManualCameraAddress(
	draft: CameraWizardDraft,
	address: string
): CameraWizardDraft {
	const value = address.trim();
	const error = manualCameraAddressError(value);
	if (error) throw new Error(error);
	if (value.toLocaleLowerCase().startsWith('rtsp://')) {
		const url = new URL(value);
		return {
			...draft,
			ip: url.hostname,
			mainRtspUrl: value,
			backend: 'retina',
			discoveryEvidence: 'Manual RTSP address supplied · stream not probed'
		};
	}
	return { ...draft, ip: value, discoveryEvidence: 'Manual camera address supplied' };
}

export function manualCameraAddressError(address: string): string | null {
	const value = address.trim();
	if (!value) return null;
	if (value.toLocaleLowerCase().startsWith('rtsp://')) {
		let url: URL;
		try {
			url = new URL(value);
		} catch {
			return 'Enter a valid RTSP URL.';
		}
		return url.hostname ? null : 'RTSP URL must include a camera address.';
	}
	try {
		validateAddress(value);
		return null;
	} catch (cause) {
		return cause instanceof Error ? cause.message : 'Camera address is invalid.';
	}
}

export function applyCatalogStreamHints(
	draft: CameraWizardDraft,
	hints: CameraCatalogStreamHints
): CameraWizardDraft {
	return {
		...draft,
		mainRtspUrl: hints.main_rtsp_url ?? draft.mainRtspUrl,
		subRtspUrl: hints.sub_rtsp_url ?? draft.subRtspUrl
	};
}

export function applyCatalogCameraDefaults(
	draft: CameraWizardDraft,
	camera: CameraCatalogCamera
): CameraWizardDraft {
	if (camera.brand.toLocaleLowerCase() !== 'reolink' || draft.backend !== 'auto') return draft;
	return {
		...draft,
		backend: 'reo-proto',
		onvifPort: draft.onvifPort || '8000',
		httpPort: draft.httpPort || '80'
	};
}

export function exactCatalogCameraMatch(
	cameras: readonly CameraCatalogCamera[],
	manufacturer: string | null,
	model: string | null
): CameraCatalogCamera | null {
	const normalizedModel = normalizeCameraIdentity(model);
	if (!normalizedModel) return null;
	const modelMatches = cameras.filter((camera) =>
		[camera.model, ...camera.aliases].some(
			(candidate) => normalizeCameraIdentity(candidate) === normalizedModel
		)
	);
	if (modelMatches.length === 1) return modelMatches[0];

	const normalizedManufacturer = normalizeCameraIdentity(manufacturer);
	if (!normalizedManufacturer) return null;
	const brandMatches = modelMatches.filter(
		(camera) => normalizeCameraIdentity(camera.brand) === normalizedManufacturer
	);
	return brandMatches.length === 1 ? brandMatches[0] : null;
}

export function firstHttpCameraCatalogSource(sources: readonly string[]): string | null {
	for (const source of sources) {
		try {
			const url = new URL(source);
			if (url.protocol === 'https:' || url.protocol === 'http:') return url.href;
		} catch {
			continue;
		}
	}
	return null;
}

function normalizeCameraIdentity(value: string | null): string {
	return (value ?? '').toLocaleLowerCase().replaceAll(/[^a-z0-9]/g, '');
}

export function validateCameraWizardStep(
	step: CameraWizardStep,
	draft: CameraWizardDraft
): string | null {
	if (step === 'find') {
		if (!draft.ip.trim()) return 'Choose a discovered camera or enter an address.';
		try {
			validateAddress(draft.ip.trim());
		} catch (cause) {
			return cause instanceof Error ? cause.message : 'Camera address is invalid.';
		}
	}
	if (step === 'connect') {
		if (!draft.username.trim() && !draft.defaultUsernameConfigured) {
			return 'Username is required.';
		}
		if (!draft.password && !draft.defaultPasswordConfigured) return 'Password is required.';
		try {
			parsePort(draft.onvifPort, 'ONVIF port');
			parsePort(draft.httpPort, 'HTTP port');
		} catch (cause) {
			return cause instanceof Error ? cause.message : 'Camera ports are invalid.';
		}
	}
	if (step === 'streams') {
		for (const [label, value] of [
			['Main RTSP URL', draft.mainRtspUrl],
			['Sub RTSP URL', draft.subRtspUrl]
		] as const) {
			if (value && !value.toLocaleLowerCase().startsWith('rtsp://')) {
				return `${label} must start with rtsp://.`;
			}
		}
	}
	if (step === 'recording') {
		if (!draft.displayName.trim()) return 'Camera name is required.';
		if (draft.recordingMode === 'event-boost') {
			try {
				parseWholeNumber(draft.eventRecordingDurationSeconds, 'Event recording duration', 1, 3_600);
			} catch (cause) {
				return cause instanceof Error ? cause.message : 'Event recording duration is invalid.';
			}
		}
	}
	return null;
}

export function cameraStreamVerificationError(
	draft: CameraWizardDraft,
	probe: CameraStreamProbeResult | null
): string | null {
	if (!probe) return 'Verify the camera streams before continuing.';
	const verified = new Set(
		probe.streams.filter((stream) => stream.verified).map((stream) => stream.stream)
	);
	const required =
		draft.recordingMode === 'event-boost' || draft.recordingMode === 'both'
			? (['main', 'sub'] as const)
			: draft.recordingMode === 'sub'
				? (['sub'] as const)
				: draft.recordingMode === 'main'
					? (['main'] as const)
					: ([] as const);
	if (required.length === 0 && verified.size === 0) {
		return 'Verify at least one camera stream before saving a camera with recording off.';
	}
	const missing = required.filter((stream) => !verified.has(stream));
	if (missing.length > 0) {
		return `Verify the ${missing.join(' and ')} stream${missing.length === 1 ? '' : 's'} required by ${recordingModeLabel(draft.recordingMode)}.`;
	}
	return null;
}

export function cameraWizardUpdate(draft: CameraWizardDraft): CameraSettingsUpdate {
	for (const step of cameraWizardSteps.slice(0, 4)) {
		const error = validateCameraWizardStep(step, draft);
		if (error) throw new Error(error);
	}
	return {
		display_name: draft.displayName.trim(),
		...(draft.username.trim() ? { username: draft.username.trim() } : {}),
		...(draft.password ? { password: draft.password } : {}),
		onvif_port: parsePort(draft.onvifPort, 'ONVIF port'),
		http_port: parsePort(draft.httpPort, 'HTTP port'),
		main_rtsp_url: draft.mainRtspUrl.trim() || null,
		sub_rtsp_url: draft.subRtspUrl.trim() || null,
		uid: null,
		backend: draft.backend,
		transport: draft.transport,
		record_generic_motion_events: draft.recordGenericMotionEvents,
		recording_mode: draft.recordingMode,
		event_recording_duration_secs: parseWholeNumber(
			draft.eventRecordingDurationSeconds,
			'Event recording duration',
			1,
			3_600
		)
	};
}

function recordingModeLabel(mode: CameraRecordingMode): string {
	if (mode === 'event-boost') return 'event boost';
	if (mode === 'both') return 'main + sub recording';
	if (mode === 'main') return 'main-only recording';
	if (mode === 'sub') return 'sub-only recording';
	return 'recording off';
}

function validateAddress(value: string): void {
	const octets = value.split('.');
	if (
		octets.length !== 4 ||
		octets.some((octet) => !/^\d+$/.test(octet) || Number(octet) < 0 || Number(octet) > 255)
	) {
		throw new Error('Enter a valid IPv4 camera address.');
	}
}

function parsePort(value: string, label: string): number | null {
	if (!value.trim()) return null;
	if (!/^\d+$/.test(value.trim())) throw new Error(`${label} must be a whole number.`);
	const port = Number(value);
	if (port < 1 || port > 65_535) throw new Error(`${label} must be between 1 and 65535.`);
	return port;
}

function parseWholeNumber(value: string, label: string, minimum: number, maximum: number): number {
	const normalized = value.trim();
	if (!/^\d+$/.test(normalized)) throw new Error(`${label} must be a whole number.`);
	const number = Number(normalized);
	if (!Number.isSafeInteger(number) || number < minimum || number > maximum) {
		throw new Error(`${label} must be between ${minimum} and ${maximum}.`);
	}
	return number;
}
