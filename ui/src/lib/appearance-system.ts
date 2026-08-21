import type { ServerHealthResponse } from '$lib/types';

export type ThemePreference = 'dark' | 'light' | 'system';
export type EffectiveTheme = 'dark' | 'light';

export function normalizeThemePreference(value: string | null): ThemePreference {
	return value === 'light' || value === 'system' ? value : 'dark';
}

export function resolveEffectiveTheme(
	preference: ThemePreference,
	prefersDark: boolean
): EffectiveTheme {
	return preference === 'system' ? (prefersDark ? 'dark' : 'light') : preference;
}

export type AppearanceSystemEvidence = {
	timeZone: null;
	clockPreference: null;
	weekStartPreference: null;
	language: 'English';
	reduceMotionPreference: null;
	system: {
		version: string | null;
		hostName: string | null;
		operatingSystem: string | null;
		uptimeSeconds: number | null;
		processName: string | null;
		executable: string | null;
		workingDirectory: string | null;
	};
	updateChannel: null;
	updateCheckRuntime: null;
	configFilePath: null;
	configBackupRuntime: null;
	eraseRecordingsRuntime: null;
	diagnosticsBundleRuntime: null;
	logsRoute: '/settings/logs';
	restartRuntime: 'implemented';
};

export function appearanceSystemEvidence(
	health: ServerHealthResponse | null
): AppearanceSystemEvidence {
	return {
		timeZone: null,
		clockPreference: null,
		weekStartPreference: null,
		language: 'English',
		reduceMotionPreference: null,
		system: {
			version: health?.version ?? null,
			hostName: health?.system?.host_name ?? null,
			operatingSystem: health?.system
				? (health.system.os_version ?? health.system.os_name ?? null)
				: null,
			uptimeSeconds: health?.uptime_seconds ?? null,
			processName: health?.system?.process?.name ?? null,
			executable: health?.system?.process?.executable ?? null,
			workingDirectory: health?.system?.process?.working_directory ?? null
		},
		updateChannel: null,
		updateCheckRuntime: null,
		configFilePath: null,
		configBackupRuntime: null,
		eraseRecordingsRuntime: null,
		diagnosticsBundleRuntime: null,
		logsRoute: '/settings/logs',
		restartRuntime: 'implemented'
	};
}
