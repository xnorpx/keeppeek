import type {
	CameraBackend,
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
	onvifPort: string;
	httpPort: string;
	mainRtspUrl: string;
	subRtspUrl: string;
	backend: CameraBackend;
	transport: CameraTransport;
	discoveryEvidence: string | null;
};

export function emptyCameraWizardDraft(): CameraWizardDraft {
	return {
		ip: '',
		displayName: '',
		username: '',
		password: '',
		onvifPort: '',
		httpPort: '',
		mainRtspUrl: '',
		subRtspUrl: '',
		backend: 'auto',
		transport: 'tcp',
		discoveryEvidence: null
	};
}

export function draftFromDiscoveredCamera(camera: DiscoveredCameraSettings): CameraWizardDraft {
	const reolink = camera.brand.toLocaleLowerCase() === 'reolink';
	return {
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
}

export function applyManualCameraAddress(
	draft: CameraWizardDraft,
	address: string
): CameraWizardDraft {
	const value = address.trim();
	if (value.toLocaleLowerCase().startsWith('rtsp://')) {
		let url: URL;
		try {
			url = new URL(value);
		} catch {
			throw new Error('Enter a valid RTSP URL.');
		}
		if (!url.hostname) throw new Error('RTSP URL must include a camera address.');
		return {
			...draft,
			ip: url.hostname,
			mainRtspUrl: value,
			backend: 'retina',
			discoveryEvidence: 'Manual RTSP address supplied · stream not probed'
		};
	}
	validateAddress(value);
	return { ...draft, ip: value, discoveryEvidence: 'Manual camera address supplied' };
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
		if (!draft.username.trim()) return 'Username is required.';
		if (!draft.password) return 'Password is required.';
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
	if (step === 'recording' && !draft.displayName.trim()) return 'Camera name is required.';
	return null;
}

export function cameraWizardUpdate(draft: CameraWizardDraft): CameraSettingsUpdate {
	for (const step of cameraWizardSteps.slice(0, 4)) {
		const error = validateCameraWizardStep(step, draft);
		if (error) throw new Error(error);
	}
	return {
		display_name: draft.displayName.trim(),
		username: draft.username.trim(),
		password: draft.password,
		onvif_port: parsePort(draft.onvifPort, 'ONVIF port'),
		http_port: parsePort(draft.httpPort, 'HTTP port'),
		main_rtsp_url: draft.mainRtspUrl.trim() || null,
		sub_rtsp_url: draft.subRtspUrl.trim() || null,
		uid: null,
		backend: draft.backend,
		transport: draft.transport
	};
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
