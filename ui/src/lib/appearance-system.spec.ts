import { describe, expect, it } from 'vitest';
import {
	appearanceSystemEvidence,
	normalizeThemePreference,
	resolveEffectiveTheme
} from '$lib/appearance-system';
import type { ServerHealthResponse } from '$lib/types';

describe('appearance and system evidence', () => {
	it('normalizes persisted theme choices and resolves the system preference', () => {
		expect(normalizeThemePreference('light')).toBe('light');
		expect(normalizeThemePreference('system')).toBe('system');
		expect(normalizeThemePreference('unexpected')).toBe('dark');
		expect(resolveEffectiveTheme('system', true)).toBe('dark');
		expect(resolveEffectiveTheme('system', false)).toBe('light');
		expect(resolveEffectiveTheme('dark', false)).toBe('dark');
	});

	it('reports only system fields present in server health', () => {
		const health = {
			version: '0.4.1-test',
			uptime_seconds: 183_840,
			system: {
				host_name: 'keeppeek.local',
				os_name: 'macOS',
				os_version: 'macOS 15.4',
				process: {
					name: 'keeppeek',
					executable: '/opt/keeppeek/bin/keeppeek',
					working_directory: '/opt/keeppeek'
				}
			}
		} as ServerHealthResponse;

		expect(appearanceSystemEvidence(health).system).toEqual({
			version: '0.4.1-test',
			hostName: 'keeppeek.local',
			operatingSystem: 'macOS 15.4',
			uptimeSeconds: 183_840,
			processName: 'keeppeek',
			executable: '/opt/keeppeek/bin/keeppeek',
			workingDirectory: '/opt/keeppeek'
		});
	});

	it('keeps absent preferences and system commands unavailable', () => {
		expect(appearanceSystemEvidence(null)).toMatchObject({
			timeZone: null,
			clockPreference: null,
			weekStartPreference: null,
			language: 'English',
			reduceMotionPreference: null,
			updateChannel: null,
			updateCheckRuntime: null,
			configFilePath: null,
			configBackupRuntime: null,
			eraseRecordingsRuntime: null,
			diagnosticsBundleRuntime: null,
			logsRoute: '/settings/logs',
			restartRuntime: 'implemented'
		});
	});

	it('fails closed when a health fixture omits process identity', () => {
		const evidence = appearanceSystemEvidence({
			system: { disks: [] },
			storage: { catalog: null }
		} as unknown as ServerHealthResponse);

		expect(evidence.system).toMatchObject({
			version: null,
			hostName: null,
			operatingSystem: null,
			processName: null,
			executable: null,
			workingDirectory: null
		});
	});
});
