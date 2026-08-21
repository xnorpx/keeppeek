import { describe, expect, it } from 'vitest';
import {
	filterMobileSettingsSections,
	mobileSettingsFocus,
	mobileSettingsSections
} from '$lib/mobile-settings';

describe('mobile settings navigation', () => {
	it('lists all ten authored administration sections', () => {
		expect(mobileSettingsSections.map((section) => section.label)).toEqual([
			'Camera defaults',
			'Storage & retention',
			'Event sources',
			'Groups',
			'Notifications',
			'Access',
			'Integrations',
			'Appearance & time',
			'System & updates',
			'Logs & diagnostics'
		]);
	});

	it('searches labels and domain keywords case-insensitively', () => {
		expect(filterMobileSettingsSections('MQTT').map((section) => section.id)).toEqual([
			'integrations'
		]);
		expect(filterMobileSettingsSections('credentials').map((section) => section.id)).toEqual([
			'camera-defaults'
		]);
		expect(filterMobileSettingsSections('  ')).toHaveLength(10);
	});

	it('maps system to the shared appearance renderer and keeps logs on its route', () => {
		expect(mobileSettingsFocus('#system')).toMatchObject({
			label: 'System & updates',
			renderTarget: 'appearance'
		});
		expect(mobileSettingsFocus('#camera-defaults')?.renderTarget).toBe('camera-defaults');
		expect(mobileSettingsFocus('#logs')).toBeNull();
		expect(mobileSettingsFocus('')).toBeNull();
	});
});
