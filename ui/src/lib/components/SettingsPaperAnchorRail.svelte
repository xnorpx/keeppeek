<script lang="ts">
	type SectionId =
		| 'access'
		| 'appearance'
		| 'camera-defaults'
		| 'event-sources'
		| 'groups'
		| 'integrations'
		| 'logs'
		| 'notifications'
		| 'storage'
		| 'system';
	type Props = {
		active: SectionId;
	};

	let { active }: Props = $props();
	const sections: Array<{ id: SectionId; label: string }> = [
		{ id: 'storage', label: 'Storage & retention' },
		{ id: 'camera-defaults', label: 'Camera defaults' },
		{ id: 'event-sources', label: 'Event sources' },
		{ id: 'groups', label: 'Groups' },
		{ id: 'integrations', label: 'Integrations' },
		{ id: 'notifications', label: 'Notifications' },
		{ id: 'access', label: 'Access' },
		{ id: 'appearance', label: 'Appearance & time' },
		{ id: 'system', label: 'System & updates' },
		{ id: 'logs', label: 'Logs & diagnostics' }
	];
</script>

<nav
	data-settings-paper-anchor-rail
	class="flex h-full w-60 shrink-0 flex-col border-r border-hairline bg-ground py-5"
	aria-label="Settings sections preview"
>
	<p class="px-5 pb-3 font-mono text-2xs leading-[14px] tracking-[0.14em] text-text-faint">
		ON THIS PAGE
	</p>
	{#each sections as section (section.id)}
		<a
			href={`#${section.id}`}
			aria-current={active === section.id ? 'location' : undefined}
			class="flex h-9 shrink-0 items-center border-l-2 px-5 text-sm leading-[18px] {active ===
			section.id
				? 'border-primary bg-primary/10 font-semibold text-foreground'
				: 'border-transparent text-text-muted'}"
		>
			{section.label}
		</a>
	{/each}
</nav>
