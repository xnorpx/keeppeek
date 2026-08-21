import { describe, expect, it } from 'vitest';
import type { CameraListItem } from './types';
import { presentCameraControl } from './camera-control';

function camera(ptz: boolean): CameraListItem {
	return {
		id: 'front-door',
		ip: '192.0.2.1',
		name: 'Front Door',
		manufacturer: null,
		model: null,
		firmware_version: null,
		is_reolink: false,
		capabilities: {
			ptz,
			audio: false,
			events: false,
			recording: true,
			analytics: false,
			imaging: false,
			two_way_audio: false
		},
		profiles: []
	};
}

describe('Camera control presentation', () => {
	it('omits PTZ when the camera does not report support', () => {
		expect(presentCameraControl(camera(false), true)).toEqual({
			showPtz: false,
			commandAvailable: false,
			reason: null
		});
	});

	it('shows hardware evidence but disables commands without a browser transport', () => {
		expect(presentCameraControl(camera(true), false)).toEqual({
			showPtz: true,
			commandAvailable: false,
			reason: 'PTZ reported · browser control transport unavailable'
		});
	});

	it('enables commands only when both hardware and transport are available', () => {
		expect(presentCameraControl(camera(true), true)).toEqual({
			showPtz: true,
			commandAvailable: true,
			reason: null
		});
	});
});
