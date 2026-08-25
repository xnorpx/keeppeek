export type MobileSettingsRenderTarget =
	| 'access'
	| 'appearance'
	| 'event-sources'
	| 'groups'
	| 'integrations'
	| 'notifications'
	| 'storage';

export type MobileSettingsSection = {
	id: MobileSettingsRenderTarget | 'system' | 'logs';
	label: string;
	group: 'administration' | 'system';
	href: string;
	renderTarget: MobileSettingsRenderTarget | null;
	keywords: readonly string[];
};

export const mobileSettingsSections = Object.freeze<MobileSettingsSection[]>([
	{
		id: 'storage',
		label: 'Storage & retention',
		group: 'administration',
		href: '#storage',
		renderTarget: 'storage',
		keywords: ['storage', 'retention', 'disk', 'archive', 'recordings']
	},
	{
		id: 'event-sources',
		label: 'Event sources',
		group: 'administration',
		href: '#event-sources',
		renderTarget: 'event-sources',
		keywords: ['events', 'sources', 'publishers', 'catalog']
	},
	{
		id: 'groups',
		label: 'Groups',
		group: 'administration',
		href: '#groups',
		renderTarget: 'groups',
		keywords: ['groups', 'audio', 'members', 'talk']
	},
	{
		id: 'notifications',
		label: 'Notifications',
		group: 'administration',
		href: '#notifications',
		renderTarget: 'notifications',
		keywords: ['notifications', 'rules', 'quiet hours', 'push', 'email']
	},
	{
		id: 'access',
		label: 'Access',
		group: 'administration',
		href: '#access',
		renderTarget: 'access',
		keywords: ['access', 'roles', 'people', 'tokens', 'identity']
	},
	{
		id: 'integrations',
		label: 'Integrations',
		group: 'system',
		href: '#integrations',
		renderTarget: 'integrations',
		keywords: ['integrations', 'home assistant', 'mqtt', 'webhooks', 'prometheus']
	},
	{
		id: 'appearance',
		label: 'Appearance & time',
		group: 'system',
		href: '#appearance',
		renderTarget: 'appearance',
		keywords: ['appearance', 'theme', 'time', 'timezone', 'language']
	},
	{
		id: 'system',
		label: 'System & updates',
		group: 'system',
		href: '#system',
		renderTarget: 'appearance',
		keywords: ['system', 'updates', 'version', 'restart', 'config']
	},
	{
		id: 'logs',
		label: 'Logs & diagnostics',
		group: 'system',
		href: '/settings/logs',
		renderTarget: null,
		keywords: ['logs', 'diagnostics', 'browser', 'server', 'export']
	}
]);

export function filterMobileSettingsSections(query: string): MobileSettingsSection[] {
	const normalized = query.trim().toLocaleLowerCase();
	if (!normalized) return [...mobileSettingsSections];
	return mobileSettingsSections.filter((section) =>
		[section.label, ...section.keywords].some((value) =>
			value.toLocaleLowerCase().includes(normalized)
		)
	);
}

export function mobileSettingsFocus(hash: string): MobileSettingsSection | null {
	const id = hash.startsWith('#') ? hash.slice(1) : hash;
	return (
		mobileSettingsSections.find((section) => section.id === id && section.renderTarget) ?? null
	);
}
