import type { CameraBackend, CameraSettings, CameraTransport } from '$lib/types';

export type CameraDefaultsEvidence = {
	cameraCount: number;
	credentials: {
		complete: number;
		partial: number;
		missing: number;
	};
	backends: Record<CameraBackend, number>;
	transports: Record<CameraTransport, number>;
	manualStreamOverrides: number;
	sharedLogin: null;
	credentialOverrides: null;
};

export function cameraDefaultsEvidence(cameras: readonly CameraSettings[]): CameraDefaultsEvidence {
	const evidence: CameraDefaultsEvidence = {
		cameraCount: cameras.length,
		credentials: { complete: 0, partial: 0, missing: 0 },
		backends: { auto: 0, retina: 0, 'reo-proto': 0 },
		transports: { tcp: 0, udp: 0 },
		manualStreamOverrides: 0,
		sharedLogin: null,
		credentialOverrides: null
	};

	for (const camera of cameras) {
		if (camera.username_configured && camera.password_configured) {
			evidence.credentials.complete += 1;
		} else if (camera.username_configured || camera.password_configured) {
			evidence.credentials.partial += 1;
		} else {
			evidence.credentials.missing += 1;
		}
		evidence.backends[camera.backend] += 1;
		evidence.transports[camera.transport] += 1;
		if (camera.main_rtsp_url !== null || camera.sub_rtsp_url !== null) {
			evidence.manualStreamOverrides += 1;
		}
	}

	return evidence;
}
