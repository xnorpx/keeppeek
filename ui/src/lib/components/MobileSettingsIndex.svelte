<script lang="ts">
	import { useAppearanceState } from '$lib/appearance-context';
	import { filterMobileSettingsSections, type MobileSettingsSection } from '$lib/mobile-settings';
	import type { SanitizedConfig } from '$lib/types';
	import SearchIcon from '@lucide/svelte/icons/search';

	type Props = {
		config: SanitizedConfig;
		backupAvailable?: boolean;
	};

	let { config, backupAvailable = false }: Props = $props();
	const appearance = useAppearanceState();
	let query = $state('');
	let filtered = $derived(filterMobileSettingsSections(query, backupAvailable));
	let administration = $derived(filtered.filter((section) => section.group === 'administration'));
	let system = $derived(filtered.filter((section) => section.group === 'system'));

	function status(section: MobileSettingsSection): string {
		switch (section.id) {
			case 'dashboards':
				return '—';
			case 'storage':
				return config.recording_estimate.estimated_retention_days === null
					? '—'
					: `${Math.round(config.recording_estimate.estimated_retention_days)} days`;
			case 'event-sources':
			case 'backups':
			case 'groups':
			case 'notifications':
			case 'access':
				return '—';
			case 'integrations':
			case 'appearance':
			case 'system':
			case 'logs':
				return '';
		}
	}

	function indicatorClass(section: MobileSettingsSection): string {
		switch (section.id) {
			case 'dashboards':
				return 'rounded-sm bg-activity';
			case 'storage':
				return 'rounded-sm bg-primary-deep';
			case 'backups':
				return 'rounded-sm bg-live';
			case 'event-sources':
				return 'rounded-full bg-healthy';
			case 'groups':
				return 'rounded-sm bg-availability';
			case 'notifications':
				return 'rounded-full bg-activity';
			case 'access':
				return 'rounded-full bg-primary-soft';
			default:
				return 'rounded-sm bg-primary';
		}
	}
</script>

<div class="flex flex-col gap-3 p-4" data-mobile-settings-index>
	<label class="relative block" for="mobile-settings-search">
		<span class="sr-only">Search settings</span>
		<SearchIcon
			class="pointer-events-none absolute top-1/2 left-[11px] size-3.5 -translate-y-1/2 text-text-faint"
		/>
		<input
			id="mobile-settings-search"
			class="h-[38px] w-full rounded-sm border border-hairline-strong bg-raised pr-[11px] pl-[33px] text-sm leading-4 outline-none focus:border-ring focus:ring-1 focus:ring-ring"
			bind:value={query}
			placeholder="Search settings"
			autocomplete="off"
		/>
	</label>

	{#if filtered.length === 0}
		<div
			class="grid min-h-40 place-items-center rounded-md border border-dashed border-hairline-strong px-4 text-center"
			role="status"
		>
			<div>
				<p class="text-sm font-medium">No settings sections match</p>
				<p class="mt-1 text-xs text-text-muted">Try a storage, access, or system term.</p>
			</div>
		</div>
	{:else}
		<nav class="flex flex-col gap-3" aria-label="Settings sections">
			{#if administration.length > 0}
				<div class="contents">
					<p class="font-mono text-2xs leading-3 tracking-[0.1em] text-text-faint">
						ADMINISTRATION
					</p>
					<div
						class="divide-y divide-hairline overflow-hidden rounded-md border border-hairline bg-surface"
					>
						{#each administration as section (section.id)}
							<a
								href={section.href}
								class="flex h-[52px] items-center gap-[11px] px-[14px] focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset"
							>
								<span class="size-2 shrink-0 {indicatorClass(section)}"></span>
								<span class="min-w-0 flex-1 text-md leading-[18px]">{section.label}</span>
								<span
									class="max-w-32 truncate font-mono text-2xs leading-3 {section.id === 'storage'
										? 'text-activity uppercase'
										: 'text-text-faint'}">{status(section)}</span
								>
							</a>
						{/each}
					</div>
				</div>
			{/if}

			{#if system.length > 0}
				<div class="contents">
					<p class="font-mono text-2xs leading-3 tracking-[0.1em] text-text-faint">SYSTEM</p>
					<div
						class="divide-y divide-hairline overflow-hidden rounded-md border border-hairline bg-surface"
					>
						{#each system as section (section.id)}
							<a
								href={section.href}
								class="flex h-[46px] items-center px-[14px] focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none focus-visible:ring-inset"
							>
								<span class="min-w-0 flex-1 text-md leading-[18px]">{section.label}</span>
							</a>
						{/each}
					</div>
				</div>
			{/if}
		</nav>
	{/if}
</div>
