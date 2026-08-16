<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import type { Snippet } from 'svelte';
	import * as Sidebar from '$lib/components/ui/sidebar/index.js';
	import EyeIcon from '@lucide/svelte/icons/eye';
	import ArchiveIcon from '@lucide/svelte/icons/archive';
	import SettingsIcon from '@lucide/svelte/icons/settings';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import MoonIcon from '@lucide/svelte/icons/moon';
	import SunIcon from '@lucide/svelte/icons/sun';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { setLivePeer } from '$lib/stream-peer-context';
	import { initializeBrowserLogging } from '$lib/browser-logs';

	initializeBrowserLogging();

	let { children }: { children: Snippet } = $props();
	let sidebarOpen = $state(false);
	let theme = $state<'dark' | 'light'>('dark');
	const livePeer = setLivePeer();

	onMount(() => {
		theme = document.documentElement.classList.contains('dark') ? 'dark' : 'light';
		const close = () => livePeer.closeOnPageHide();
		window.addEventListener('pagehide', close);
		return () => {
			window.removeEventListener('pagehide', close);
			close();
		};
	});

	function toggleTheme() {
		theme = theme === 'dark' ? 'light' : 'dark';
		document.documentElement.classList.toggle('dark', theme === 'dark');
		document.documentElement.dataset.theme = theme;
		try {
			localStorage.setItem('keeppeek-theme', theme);
		} catch {
			// The active theme still applies when storage is unavailable.
		}
	}

	const navigation = [
		{ href: '/', label: 'Peek', icon: EyeIcon },
		{ href: '/keep', label: 'Keep', icon: ArchiveIcon },
		{ href: '/system-health', label: 'Health', icon: ActivityIcon },
		{ href: '/settings', label: 'Settings', icon: SettingsIcon }
	] as const;

	let currentRoute = $derived(
		navigation.find((item) =>
			item.href === '/settings'
				? page.url.pathname.startsWith('/settings')
				: item.href === page.url.pathname
		) ?? navigation[0]
	);
	let settingsActive = $derived(page.url.pathname.startsWith('/settings'));

	$effect.pre(() => {
		if (page.url.pathname !== '/system-health') return;
		return livePeer.hold();
	});
</script>

<Sidebar.Provider
	bind:open={sidebarOpen}
	style="--sidebar-width: 13rem; --sidebar-width-icon: 3.5rem;"
>
	<Sidebar.Root collapsible="icon" class="z-40 border-r-0">
		<Sidebar.Header>
			<Sidebar.Menu>
				<Sidebar.MenuItem>
					<Sidebar.MenuButton size="lg" tooltipContent="KeepPeek home">
						{#snippet child({ props })}
							<a href={resolve('/')} {...props}>
								<div
									class="flex aspect-square size-8 items-center justify-center rounded-md bg-sidebar-primary text-sidebar-primary-foreground"
								>
									<CameraIcon class="size-4" />
								</div>
								<div
									class="grid flex-1 text-left text-sm leading-tight group-data-[collapsible=icon]:hidden"
								>
									<span class="truncate font-semibold">KeepPeek</span>
									<span class="truncate text-xs text-sidebar-foreground/55">Network video</span>
								</div>
							</a>
						{/snippet}
					</Sidebar.MenuButton>
				</Sidebar.MenuItem>
			</Sidebar.Menu>
		</Sidebar.Header>

		<Sidebar.Content>
			<Sidebar.Group>
				<Sidebar.GroupContent>
					<Sidebar.Menu>
						{#each navigation.slice(0, 3) as item (item.href)}
							<Sidebar.MenuItem>
								<Sidebar.MenuButton
									isActive={page.url.pathname === item.href}
									tooltipContent={item.label}
								>
									{#snippet child({ props })}
										<a
											href={resolve(item.href)}
											{...props}
											aria-current={page.url.pathname === item.href ? 'page' : undefined}
										>
											<item.icon />
											<span>{item.label}</span>
										</a>
									{/snippet}
								</Sidebar.MenuButton>
							</Sidebar.MenuItem>
						{/each}
					</Sidebar.Menu>
				</Sidebar.GroupContent>
			</Sidebar.Group>
		</Sidebar.Content>

		<Sidebar.Footer>
			<Sidebar.Menu>
				<Sidebar.MenuItem>
					<Sidebar.MenuButton isActive={settingsActive} tooltipContent="Settings">
						{#snippet child({ props })}
							<a
								href={resolve('/settings')}
								{...props}
								aria-current={settingsActive ? 'page' : undefined}
							>
								<SettingsIcon />
								<span>Settings</span>
							</a>
						{/snippet}
					</Sidebar.MenuButton>
				</Sidebar.MenuItem>
			</Sidebar.Menu>
		</Sidebar.Footer>

		<Sidebar.Rail />
	</Sidebar.Root>

	<Sidebar.Inset class="min-w-0 pb-16 md:pb-0">
		<header
			class="flex h-12 shrink-0 items-center gap-3 border-b border-white/10 bg-sidebar px-3 text-sidebar-foreground md:px-4"
		>
			<Sidebar.Trigger
				class="hidden text-white hover:bg-white/10 hover:text-white md:inline-flex"
			/>
			<a
				href={resolve('/')}
				class="flex items-center gap-2 font-semibold md:hidden"
				aria-label="KeepPeek home"
			>
				<span class="grid size-7 place-items-center rounded-md bg-sidebar-primary text-white">
					<CameraIcon class="size-3.5" />
				</span>
				<span>KeepPeek</span>
			</a>
			<span class="hidden h-4 w-px bg-white/10 md:block"></span>
			<span class="hidden text-sm font-medium text-white/60 md:inline">{currentRoute.label}</span>
			<span class="ml-auto flex items-center gap-2 text-xs text-white/50">
				<span class="size-1.5 rounded-full bg-emerald-500"></span>
				Local
			</span>
			<button
				type="button"
				class="grid size-8 place-items-center rounded-md text-white/55 hover:bg-white/10 hover:text-white focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-none"
				aria-label={theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
				title={theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
				onclick={toggleTheme}
			>
				{#if theme === 'dark'}
					<SunIcon class="size-4" />
				{:else}
					<MoonIcon class="size-4" />
				{/if}
			</button>
		</header>
		<main
			class="flex-1 overflow-auto p-3 md:p-4 {page.url.pathname === '/'
				? 'live-surface'
				: 'workspace-surface'}"
		>
			{@render children()}
		</main>
	</Sidebar.Inset>

	<nav
		class="fixed inset-x-0 bottom-0 z-50 grid h-16 grid-cols-4 border-t border-white/10 bg-sidebar/95 px-3 text-sidebar-foreground backdrop-blur md:hidden"
		aria-label="Primary navigation"
	>
		{#each navigation as item (item.href)}
			<a
				href={resolve(item.href)}
				aria-current={(item.href === '/settings' ? settingsActive : page.url.pathname === item.href)
					? 'page'
					: undefined}
				class="flex min-w-0 flex-col items-center justify-center gap-1 text-[11px] font-medium {(
					item.href === '/settings' ? settingsActive : page.url.pathname === item.href
				)
					? 'text-white'
					: 'text-white/45'}"
			>
				<item.icon class="size-4" />
				<span>{item.label}</span>
			</a>
		{/each}
	</nav>
</Sidebar.Provider>
