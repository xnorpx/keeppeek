import { describe, expect, it } from 'vitest';
import {
	applyCatalogStreamHints,
	applyCatalogCameraDefaults,
	applyManualCameraAddress,
	cameraStreamVerificationError,
	cameraWizardUpdate,
	draftFromDiscoveredCamera,
	emptyCameraWizardDraft,
	exactCatalogCameraMatch,
	firstHttpCameraCatalogSource,
	manualCameraAddressError,
	validateCameraWizardStep
} from './camera-wizard';
import type { CameraCatalogCamera, CameraStreamProbeResult } from './types';

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

	it('selects only a unique exact ONVIF model match from the catalog', () => {
		expect(exactCatalogCameraMatch([catalogCamera], 'Manufacturer', 'rlc test')).toBe(
			catalogCamera
		);

		const otherBrand = { ...catalogCamera, id: 'other-rlc-test', brand: 'Other' };
		expect(exactCatalogCameraMatch([catalogCamera, otherBrand], 'Reolink', 'RLC-Test')).toBe(
			catalogCamera
		);
		expect(exactCatalogCameraMatch([catalogCamera, otherBrand], null, 'RLC-Test')).toBeNull();
	});

	it('selects the first valid web source from a matched catalog camera', () => {
		expect(
			firstHttpCameraCatalogSource([
				'onvif',
				'rtsp://192.0.2.77/main',
				'https://www.cctv-database.com/camera/annke-fcd800-i91et/',
				'https://example.com/secondary'
			])
		).toBe('https://www.cctv-database.com/camera/annke-fcd800-i91et/');
		expect(firstHttpCameraCatalogSource(['not a URL', 'rtsp://192.0.2.77/main'])).toBeNull();
	});

	it('uses native defaults for a matched Reolink unless the backend was explicit', () => {
		expect(applyCatalogCameraDefaults(emptyCameraWizardDraft(), catalogCamera)).toMatchObject({
			backend: 'reo-proto',
			onvifPort: '8000',
			httpPort: '80'
		});
		expect(
			applyCatalogCameraDefaults({ ...emptyCameraWizardDraft(), backend: 'retina' }, catalogCamera)
				.backend
		).toBe('retina');
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
			transport: 'tcp',
			record_generic_motion_events: false,
			recording_mode: 'event-boost',
			event_recording_duration_secs: 60
		});
	});

	it('requires verified streams selected by the recording policy', () => {
		const draft = {
			...emptyCameraWizardDraft(),
			recordingMode: 'event-boost' as const
		};
		const probe = {
			streams: [
				{
					stream: 'main' as const,
					verified: true,
					codec: 'h265',
					resolution: '3840x2160',
					declared_fps: 25,
					frames_received: 2,
					keyframe_received: true,
					elapsed_ms: 120,
					error: null
				}
			]
		} as CameraStreamProbeResult;

		expect(cameraStreamVerificationError(draft, probe)).toBe(
			'Verify the sub stream required by event boost.'
		);
		expect(
			cameraStreamVerificationError(draft, {
				...probe,
				streams: [...probe.streams, { ...probe.streams[0]!, stream: 'sub' as const, codec: 'h264' }]
			})
		).toBeNull();
	});

	it('uses configured credential defaults without adding values to the write payload', () => {
		const draft = {
			...emptyCameraWizardDraft(),
			ip: '192.0.2.77',
			displayName: 'Front Gate',
			defaultUsernameConfigured: true,
			defaultPasswordConfigured: true
		};

		expect(validateCameraWizardStep('connect', draft)).toBeNull();
		const update = cameraWizardUpdate(draft);
		expect(update).not.toHaveProperty('username');
		expect(update).not.toHaveProperty('password');
	});
});
