import { describe, expect, it } from 'vitest';
import { cameraDefaultsEvidence } from '$lib/camera-defaults';
import type { CameraSettings } from '$lib/types';

function camera(update: Partial<CameraSettings>): CameraSettings {
	return {
		id: 'camera',
		ip: '192.0.2.1',
		display_name: null,
		manufacturer_override: null,
		username_configured: true,
		password_configured: true,
		onvif_port: null,
		http_port: null,
		main_rtsp_url: null,
		sub_rtsp_url: null,
		uid_configured: false,
		backend: 'auto',
		transport: 'tcp',
		record_generic_motion_events: false,
		health: null,
		model: null,
		...update
	};
}

describe('camera defaults evidence', () => {
	it('summarizes only observable per-camera settings', () => {
		const evidence = cameraDefaultsEvidence([
			camera({ id: 'one' }),
			camera({
				id: 'two',
				username_configured: true,
				password_configured: false,
				backend: 'retina',
				transport: 'udp',
				main_rtsp_url: 'rtsp://192.0.2.2/main'
			}),
			camera({
				id: 'three',
				username_configured: false,
				password_configured: false,
				backend: 'reo-proto'
			})
		]);

		expect(evidence).toEqual({
			cameraCount: 3,
			credentials: { complete: 1, partial: 1, missing: 1 },
			backends: { auto: 1, retina: 1, 'reo-proto': 1 },
			transports: { tcp: 2, udp: 1 },
			manualStreamOverrides: 1,
			sharedLogin: null,
			credentialOverrides: null
		});
	});

	it('does not infer shared credentials from matching credential booleans', () => {
		const evidence = cameraDefaultsEvidence([
			camera({ id: 'one' }),
			camera({ id: 'two', ip: '192.0.2.2' })
		]);

		expect(evidence.credentials.complete).toBe(2);
		expect(evidence.sharedLogin).toBeNull();
		expect(evidence.credentialOverrides).toBeNull();
	});
});
