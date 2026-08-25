import { describe, expect, it } from 'vitest';
import {
	filterMobileSettingsSections,
	mobileSettingsFocus,
	mobileSettingsSections
} from '$lib/mobile-settings';

describe('mobile settings navigation', () => {
	it('lists the nine server-wide administration sections', () => {
		expect(mobileSettingsSections.map((section) => section.label)).toEqual([
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
		expect(filterMobileSettingsSections('recordings').map((section) => section.id)).toEqual([
			'storage'
		]);
		expect(filterMobileSettingsSections('  ')).toHaveLength(9);
	});

	it('maps system to the shared appearance renderer and keeps logs on its route', () => {
		expect(mobileSettingsFocus('#system')).toMatchObject({
			label: 'System & updates',
			renderTarget: 'appearance'
		});
		expect(mobileSettingsFocus('#camera-defaults')).toBeNull();
		expect(mobileSettingsFocus('#logs')).toBeNull();
		expect(mobileSettingsFocus('')).toBeNull();
	});
});
