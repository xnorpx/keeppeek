import { describe, expect, it } from 'vitest';
import {
	applyManualCameraAddress,
	cameraWizardUpdate,
	draftFromDiscoveredCamera,
	emptyCameraWizardDraft,
	validateCameraWizardStep
} from './camera-wizard';

describe('Camera wizard drafts', () => {
	it('initializes a Reolink discovery result without credentials', () => {
		const draft = draftFromDiscoveredCamera({
			ip: '192.0.2.77',
			brand: 'reolink',
			name: 'Front Gate',
			model: 'RLC-Test',
			onvif_port: null,
			sources: ['onvif', 'baichuan'],
			configured: false,
			health: null
		});

		expect(draft).toMatchObject({
			ip: '192.0.2.77',
			displayName: 'Front Gate',
			onvifPort: '8000',
			httpPort: '80',
			backend: 'reo-proto',
			username: '',
			password: ''
		});
	});

	it('extracts a camera address from a manual RTSP URL', () => {
		expect(
			applyManualCameraAddress(emptyCameraWizardDraft(), 'rtsp://192.0.2.9:8554/main')
		).toMatchObject({
			ip: '192.0.2.9',
			mainRtspUrl: 'rtsp://192.0.2.9:8554/main',
			backend: 'retina'
		});
	});

	it('validates each step without mutating the draft', () => {
		const draft = emptyCameraWizardDraft();

		expect(validateCameraWizardStep('find', draft)).toBe(
			'Choose a discovered camera or enter an address.'
		);
		expect(draft).toEqual(emptyCameraWizardDraft());
	});

	it('builds the only final write payload from a complete draft', () => {
		const update = cameraWizardUpdate({
			...emptyCameraWizardDraft(),
			ip: '192.0.2.77',
			displayName: 'Front Gate',
			username: 'operator',
			password: 'secret',
			onvifPort: '8000',
			httpPort: '80',
			mainRtspUrl: 'rtsp://192.0.2.77/main',
			subRtspUrl: 'rtsp://192.0.2.77/sub',
			backend: 'reo-proto',
			transport: 'tcp'
		});

		expect(update).toEqual({
			display_name: 'Front Gate',
			username: 'operator',
			password: 'secret',
			onvif_port: 8000,
			http_port: 80,
			main_rtsp_url: 'rtsp://192.0.2.77/main',
			sub_rtsp_url: 'rtsp://192.0.2.77/sub',
			uid: null,
			backend: 'reo-proto',
			transport: 'tcp'
		});
	});
});
