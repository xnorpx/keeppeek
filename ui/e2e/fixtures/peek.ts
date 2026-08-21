import type { Page } from '@playwright/test';
import type { CameraListItem } from '../../src/lib/types';
import { mockControlPeer, type HealthFixture } from './control-peer';

function camera(id: string, name: string): CameraListItem {
	return {
		id,
		ip: `192.0.2.${id.length}`,
		name,
		manufacturer: null,
		model: null,
		firmware_version: null,
		is_reolink: false,
		capabilities: {
			ptz: false,
			audio: false,
			events: true,
			recording: true,
			analytics: false,
			imaging: false,
			two_way_audio: false
		},
		profiles: []
	};
}

export const mixedCameras = [
	camera('front-door', 'Front Door'),
	camera('porch', 'Porch'),
	camera('alley', 'Alley'),
	camera('back-yard', 'Back Yard')
];

export const mixedHealth: HealthFixture = {
	status: 'degraded',
	cameras: [
		{
			id: 'front-door',
			state: 'online',
			lifecycle: 'Connected',
			last_error: null,
			streams: [
				{ type: 'sub', fps: 25, frames: 1_000, drops: 0, report_age_ms: 20, updated_at_ms: 1 }
			]
		},
		{
			id: 'porch',
			state: 'degraded',
			lifecycle: 'Connected',
			last_error: null,
			streams: [
				{ type: 'sub', fps: 11, frames: 86, drops: 14, report_age_ms: 4_000, updated_at_ms: 1 }
			]
		},
		{
			id: 'alley',
			state: 'stale',
			lifecycle: 'Attempt 3',
			last_error: null,
			streams: [
				{ type: 'sub', fps: 0, frames: 0, drops: 0, report_age_ms: 41_000, updated_at_ms: 1 }
			]
		},
		{
			id: 'back-yard',
			state: 'offline',
			lifecycle: 'Stopped',
			last_error: 'Authentication failed',
			streams: [{ type: 'sub', frames: 0, drops: 0, report_age_ms: 8_040_000, updated_at_ms: 1 }]
		}
	]
};

export async function mockMixedHealth(page: Page): Promise<void> {
	await mockControlPeer(page, { cameras: mixedCameras, health: mixedHealth });
}
