import { describe, expect, it } from 'vitest';
import {
	applyCatalogStreamHints,
	applyManualCameraAddress,
	cameraWizardUpdate,
	draftFromDiscoveredCamera,
	emptyCameraWizardDraft,
	manualCameraAddressError,
	validateCameraWizardStep
} from './camera-wizard';
import type { CameraCatalogCamera } from './types';

const catalogCamera: CameraCatalogCamera = {
	id: 'reolink-rlc-test',
	brand: 'Reolink',
	model: 'RLC-Test',
	aliases: [],
	camera_type: 'bullet',
	resolution_label: null,
	megapixels: null,
	sensor: null,
	field_of_view: null,
	night_vision: null,
	ip_rating: null,
	ik_rating: null,
	two_way_audio: null,
	release_year: null,
	community_notes_count: 0,
	protocols: ['onvif', 'rtsp'],
	codecs: [],
	streams: [],
	sources: [],
	stream_hints: {
		main_rtsp_url: 'rtsp://192.0.2.77/main',
		sub_rtsp_url: 'rtsp://192.0.2.77/sub'
	}
};

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

	it('automatically applies catalog streams for a discovered camera', () => {
		const draft = draftFromDiscoveredCamera({
			ip: '192.0.2.77',
			brand: 'reolink',
			name: 'Front Gate',
			model: 'RLC-Test',
			onvif_port: 8000,
			sources: ['onvif'],
			configured: false,
			health: null,
			catalog: catalogCamera
		});

		expect(draft.mainRtspUrl).toBe('rtsp://192.0.2.77/main');
		expect(draft.subRtspUrl).toBe('rtsp://192.0.2.77/sub');
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

	it('reports address errors before applying a manual draft', () => {
		expect(manualCameraAddressError('192.0.2.88')).toBeNull();
		expect(manualCameraAddressError('rtsp://192.0.2.88:8554/main')).toBeNull();
		expect(manualCameraAddressError('192.0.2')).toBe('Enter a valid IPv4 camera address.');
		expect(manualCameraAddressError('rtsp://')).toBe('RTSP URL must include a camera address.');
	});

	it('applies only supplied catalog stream hints to the draft', () => {
		const draft = applyCatalogStreamHints(
			{
				...emptyCameraWizardDraft(),
				mainRtspUrl: 'rtsp://192.0.2.77/manual-main'
			},
			{
				main_rtsp_url: null,
				sub_rtsp_url: 'rtsp://192.0.2.77/catalog-sub'
			}
		);

		expect(draft.mainRtspUrl).toBe('rtsp://192.0.2.77/manual-main');
		expect(draft.subRtspUrl).toBe('rtsp://192.0.2.77/catalog-sub');
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
