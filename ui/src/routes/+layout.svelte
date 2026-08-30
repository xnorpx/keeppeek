<script lang="ts">
	import '../app.css';
	import { goto } from '$app/navigation';
	import { onMount } from 'svelte';
	import type { Snippet } from 'svelte';
	import * as Tooltip from '$lib/components/ui/tooltip/index.js';
	import EyeIcon from '@lucide/svelte/icons/eye';
	import ArchiveIcon from '@lucide/svelte/icons/archive';
	import SettingsIcon from '@lucide/svelte/icons/settings';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import ScanLineIcon from '@lucide/svelte/icons/scan-line';
	import MoonIcon from '@lucide/svelte/icons/moon';
	import LogOutIcon from '@lucide/svelte/icons/log-out';
	import SunIcon from '@lucide/svelte/icons/sun';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { setLivePeer } from '$lib/stream-peer-context';
	import { setCapabilityState } from '$lib/capability-context';
	import { setControlClient } from '$lib/control-context';
	import { setAppearanceState } from '$lib/appearance-context';
	import { initializeBrowserLogging } from '$lib/browser-logs';
	import KeyboardOverlay from '$lib/components/KeyboardOverlay.svelte';
	import RemoteSignIn from '$lib/components/RemoteSignIn.svelte';
	import MobileNavigation from '$lib/components/MobileNavigation.svelte';
	import MobileSettingsHeader from '$lib/components/MobileSettingsHeader.svelte';
	import {
		isKeyboardTypingTarget,
		keyboardDestinations,
		resolveGlobalKeyboardAction,
		type KeyboardOverlayMode
	} from '$lib/keyboard-shortcuts';
	import type { AccessConnectionState } from '$lib/access';
	import type { ServerHealthResponse } from '$lib/types';
	import { setShellHealthPublisher } from '$lib/shell-health-context';

	initializeBrowserLogging();
	const shellHealthRefreshIntervalMs = 5_000;

	let { children }: { children: Snippet } = $props();
	const controlClient = setControlClient();
	const livePeer = setLivePeer({
		create: (offer) => controlClient.createWebRtcSession(offer),
		delete: (sessionId, options) => controlClient.deleteWebRtcSession(sessionId, options)
	});
	const appearance = setAppearanceState();
	const capabilities = setCapabilityState();
	let accessState = $state.raw<AccessConnectionState>({
		status: 'checking',
		session: null,
		message: null,
		generation: 0
	});
	let keyboardOverlay = $state<KeyboardOverlayMode>(null);
	let keyboardReady = $state(false);
	let navigationChordPending = $state(false);
	let railFocusIndex = $state(0);
	let railPathname = '';
	let navigationChordTimer: ReturnType<typeof setTimeout> | null = null;
	let shellHealth = $state.raw<ServerHealthResponse | null>(null);
	let shellHealthRequestVersion = 0;
	setShellHealthPublisher((health) => (shellHealth = health));
	let shellStatusIndicators = $derived.by(() => {
		if (shellHealth === null) {
			return [
				{ id: 'server', value: '—/1', label: 'Server health unavailable', tone: 'bg-text-faint' },
				{ id: 'cameras', value: '—/—', label: 'Camera health unavailable', tone: 'bg-text-faint' },
				{
					id: 'recording',
					value: '—/—',
					label: 'Recording health unavailable',
					tone: 'bg-text-faint'
				},
				{ id: 'clients', value: '—/—', label: 'Client health unavailable', tone: 'bg-text-faint' }
			];
		}
		const totals = shellHealth.totals;
		const activeSessions = shellHealth.webrtc.active_sessions;
		const clientSessions = shellHealth.webrtc.browser_sessions;
		return [
			{
				id: 'server',
				value: '1/1',
				label: `Server ${shellHealth.status}`,
				tone:
					shellHealth.status === 'healthy'
						? 'bg-healthy'
						: shellHealth.status === 'degraded'
							? 'bg-activity'
							: 'bg-live'
			},
			{
				id: 'cameras',
				value: `${totals.connected_cameras}/${totals.configured_cameras}`,
				label: `Cameras connected: ${totals.connected_cameras} of ${totals.configured_cameras}`,
				tone: ratioTone(totals.connected_cameras, totals.configured_cameras)
			},
			{
				id: 'recording',
				value: `${totals.recording_cameras}/${totals.recording_requested_cameras}`,
				label: `Cameras recording: ${totals.recording_cameras} of ${totals.recording_requested_cameras} requested`,
				tone: ratioTone(totals.recording_cameras, totals.recording_requested_cameras)
			},
			{
				id: 'clients',
				value: `${clientSessions}/${activeSessions}`,
				label: `Client sessions: ${clientSessions} of ${activeSessions} active WebRTC sessions`,
				tone: ratioTone(clientSessions, activeSessions)
			}
		];
	});

	function ratioTone(value: number, total: number): string {
		if (total === 0 || value >= total) return 'bg-healthy';
		if (value === 0) return 'bg-live';
		return 'bg-activity';
	}

	$effect(() => {
		if (accessState.status !== 'authenticated') {
			shellHealth = null;
			return;
		}
		if (
			page.url.pathname === '/' ||
			page.url.pathname === '/viewer' ||
			page.url.pathname === '/system-health'
		)
			return;
		const controller = new AbortController();
		const requestVersion = ++shellHealthRequestVersion;
		const refresh = async () => {
			try {
				const health = await controlClient.getHealth(controller.signal);
				if (!controller.signal.aborted && requestVersion === shellHealthRequestVersion) {
					shellHealth = health;
				}
			} catch {
				if (!controller.signal.aborted && requestVersion === shellHealthRequestVersion) {
					shellHealth = null;
				}
			}
		};
		void refresh();
		const timer = window.setInterval(() => void refresh(), shellHealthRefreshIntervalMs);
		return () => {
			controller.abort();
			window.clearInterval(timer);
		};
	});

	onMount(() => {
		const closeAppearance = appearance.initialize();
		const closeCapabilities = controlClient.onCapabilities((capabilityIds) => {
			capabilities.updateAdvertised(capabilityIds);
		});
		const closeAccessState = controlClient.onAccessState((state) => {
			accessState = state;
		});
		void controlClient.checkAccess().catch(() => undefined);
		const close = () => {
			livePeer.closeOnPageHide();
			controlClient.closeOnPageHide();
		};
		keyboardReady = true;
		window.addEventListener('keydown', handleGlobalKeyboard, { capture: true });
		window.addEventListener('pagehide', close);
		return () => {
			clearNavigationChord();
			closeAppearance();
			closeCapabilities();
			closeAccessState();
			window.removeEventListener('keydown', handleGlobalKeyboard, { capture: true });
			window.removeEventListener('pagehide', close);
			close();
		};
	});

	function toggleTheme() {
		appearance.toggleEffectiveTheme();
	}

	function clearNavigationChord(): void {
		navigationChordPending = false;
		if (navigationChordTimer !== null) clearTimeout(navigationChordTimer);
		navigationChordTimer = null;
	}

	function beginNavigationChord(): void {
		clearNavigationChord();
		navigationChordPending = true;
		navigationChordTimer = setTimeout(clearNavigationChord, 1_500);
	}

	function visibleSearchInput(): HTMLInputElement | null {
		return (
			[
				...document.querySelectorAll<HTMLInputElement>(
					'input[type="search"], input[placeholder^="Search"]'
				)
			]
				.filter((input) => !input.disabled)
				.find((input) => {
					const bounds = input.getBoundingClientRect();
					return bounds.width > 0 && bounds.height > 0;
				}) ?? null
		);
	}

	function handleGlobalKeyboard(event: KeyboardEvent): void {
		const action = resolveGlobalKeyboardAction(event, {
			overlay: keyboardOverlay,
			navigationPending: navigationChordPending,
			typing: isKeyboardTypingTarget(event.target)
		});
		if (!action) return;
		if (action.type === 'focus-search') {
			const input = visibleSearchInput();
			if (!input) return;
			event.preventDefault();
			event.stopImmediatePropagation();
			input.focus();
			input.select();
			return;
		}
		event.preventDefault();
		event.stopImmediatePropagation();
		if (action.type === 'start-navigation') {
			beginNavigationChord();
			return;
		}
		clearNavigationChord();
		if (action.type === 'cancel-navigation') return;
		if (action.type === 'open-help') {
			keyboardOverlay = 'help';
			return;
		}
		if (action.type === 'open-commands') {
			keyboardOverlay = 'commands';
			return;
		}
		if (action.type === 'close-overlay') {
			keyboardOverlay = null;
			return;
		}
		keyboardOverlay = null;
		void goto(action.href);
	}

	function navigateFromKeyboard(href: string): void {
		keyboardOverlay = null;
		clearNavigationChord();
		void goto(href);
	}

	function moveRailFocus(event: KeyboardEvent, currentIndex: number): void {
		if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
		event.preventDefault();
		const count = 1 + navigation.length + (administrator ? 1 : 0);
		railFocusIndex =
			event.key === 'ArrowDown' ? (currentIndex + 1) % count : (currentIndex - 1 + count) % count;
		document.querySelector<HTMLElement>(`[data-shell-rail-link="${railFocusIndex}"]`)?.focus();
	}

	const allNavigation = [
		{ href: '/viewer', label: 'Viewer', icon: EyeIcon, paths: ['/viewer'], administrator: false },
		{
			href: '/keep',
			label: 'Keep',
			icon: ArchiveIcon,
			paths: ['/keep', '/recordings'],
			administrator: false
		},
		{
			href: '/events',
			label: 'Events',
			icon: ScanLineIcon,
			paths: ['/events'],
			administrator: false
		},
		{
			href: '/cameras',
			label: 'Cameras',
			icon: CameraIcon,
			paths: ['/cameras'],
			administrator: true
		},
		{
			href: '/system-health',
			label: 'Health',
			icon: ActivityIcon,
			paths: ['/system-health'],
			administrator: true
		}
	] as const;
	let administrator = $derived(accessState.session?.role === 'administrator');
	let navigation = $derived(allNavigation.filter((item) => administrator || !item.administrator));

	function matchesRoute(paths: readonly string[], pathname: string): boolean {
		return paths.some((path) =>
			path === '/' ? pathname === path : pathname === path || pathname.startsWith(`${path}/`)
		);
	}

	let settingsActive = $derived(page.url.pathname.startsWith('/settings'));
	let dashboardActive = $derived(page.url.pathname === '/');
	let primaryViewActive = $derived(
		dashboardActive ||
			allNavigation.some((item) => item.paths.some((pathname) => pathname === page.url.pathname))
	);
	let setupActive = $derived(page.url.pathname === '/setup');
	let mobileCameraWizardActive = $derived(page.url.pathname === '/cameras/new');
	let mobileCameraDetailActive = $derived(page.url.pathname === '/camera');
	let healthOverviewActive = $derived(page.url.pathname === '/system-health');
	let cameraDiagnosisActive = $derived(page.url.pathname.startsWith('/system-health/camera/'));
	let mobileSettingsActionActive = $derived(settingsActive && page.url.hash === '#access');
	let mobileFocusedActionActive = $derived(
		mobileSettingsActionActive || cameraDiagnosisActive || mobileCameraWizardActive
	);
	let mobileRouteOwnsBottom = $derived(mobileFocusedActionActive || mobileCameraDetailActive);
	let currentRoute = $derived(
		setupActive
			? { label: 'First run' }
			: dashboardActive
				? { label: 'Dashboard' }
				: settingsActive
					? { label: 'Settings' }
					: (navigation.find((item) => matchesRoute(item.paths, page.url.pathname)) ??
						navigation[0])
	);
	const healthNavigation = allNavigation[4];
	let healthActive = $derived(matchesRoute(healthNavigation.paths, page.url.pathname));

	$effect(() => {
		if (accessState.status !== 'authenticated' || administrator) return;
		if (
			['/settings', '/cameras', '/system-health', '/setup'].some(
				(pathname) => page.url.pathname === pathname || page.url.pathname.startsWith(`${pathname}/`)
			)
		) {
			void goto(resolve('/'));
		}
	});

	$effect(() => {
		const pathname = page.url.pathname;
		if (pathname === railPathname) return;
		railPathname = pathname;
		railFocusIndex = dashboardActive
			? 0
			: settingsActive && administrator
				? navigation.length + 1
				: Math.max(1, navigation.findIndex((item) => matchesRoute(item.paths, pathname)) + 1);
	});

	$effect.pre(() => {
		if (page.url.pathname !== '/system-health') return;
		return livePeer.hold();
	});
</script>

{#if accessState.status === 'authenticated'}
	<Tooltip.Provider delayDuration={0}>
		<div
			data-keyboard-ready={keyboardReady}
			class="min-h-svh bg-background text-foreground md:grid md:grid-cols-[64px_minmax(0,1fr)]"
		>
			<aside
				data-shell-rail
				class="hidden min-h-svh w-16 flex-col items-center gap-2.5 border-r border-sidebar-border bg-ground py-4 text-sidebar-foreground md:flex"
				aria-label="Desktop navigation"
			>
				<a
					href={resolve('/')}
					data-shell-rail-link="0"
					tabindex={railFocusIndex === 0 ? 0 : -1}
					class="grid h-[30px] w-[34px] shrink-0 place-items-center rounded-sm bg-primary font-mono text-[10px] font-semibold text-primary-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-none"
					aria-label="Dashboard"
					aria-current={dashboardActive ? 'page' : undefined}
					onfocus={() => (railFocusIndex = 0)}
					onkeydown={(event) => moveRailFocus(event, 0)}
				>
					KP
				</a>

				<div class="h-[18px] shrink-0"></div>

				<nav class="flex w-full flex-col items-center gap-2.5" aria-label="Primary navigation">
					{#each navigation as item, index (item.href)}
						{@const active = matchesRoute(item.paths, page.url.pathname)}
						{@const railIndex = index + 1}
						<Tooltip.Root>
							<Tooltip.Trigger>
								{#snippet child({ props })}
									<a
										href={item.href}
										{...props}
										data-shell-rail-link={railIndex}
										tabindex={railFocusIndex === railIndex ? 0 : -1}
										aria-label={item.label}
										aria-current={active ? 'page' : undefined}
										class="relative grid size-11 place-items-center text-sidebar-foreground/55 transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-none focus-visible:ring-inset {active
											? 'bg-sidebar-accent text-sidebar-accent-foreground'
											: ''}"
										onfocus={() => (railFocusIndex = railIndex)}
										onkeydown={(event) => moveRailFocus(event, railIndex)}
									>
										{#if active}
											<span class="absolute inset-y-2 left-0 w-0.5 bg-primary"></span>
										{/if}
										<item.icon class="size-[18px]" strokeWidth={1.75} />
									</a>
								{/snippet}
							</Tooltip.Trigger>
							<Tooltip.Content side="right" align="center">{item.label}</Tooltip.Content>
						</Tooltip.Root>
					{/each}
					{#if administrator}<Tooltip.Root>
							<Tooltip.Trigger>
								{#snippet child({ props })}
									<a
										href={resolve('/settings')}
										{...props}
										data-shell-rail-link={navigation.length + 1}
										tabindex={railFocusIndex === navigation.length + 1 ? 0 : -1}
										aria-label="Settings"
										aria-current={settingsActive ? 'page' : undefined}
										class="relative grid size-11 place-items-center text-sidebar-foreground/55 hover:bg-sidebar-accent hover:text-sidebar-accent-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-none focus-visible:ring-inset {settingsActive
											? 'bg-sidebar-accent text-sidebar-accent-foreground'
											: ''}"
										onfocus={() => (railFocusIndex = navigation.length + 1)}
										onkeydown={(event) => moveRailFocus(event, navigation.length + 1)}
									>
										{#if settingsActive}
											<span class="absolute inset-y-2 left-0 w-0.5 bg-primary"></span>
										{/if}
										<SettingsIcon class="size-[18px]" strokeWidth={1.75} />
									</a>
								{/snippet}
							</Tooltip.Trigger>
							<Tooltip.Content side="right" align="center">Settings</Tooltip.Content>
						</Tooltip.Root>{/if}
				</nav>

				<div data-shell-rail-actions class="mt-auto flex flex-col items-center gap-2">
					<div data-shell-status-indicators class="flex flex-col items-center gap-0.5">
						{#each shellStatusIndicators as indicator (indicator.id)}
							<Tooltip.Root>
								<Tooltip.Trigger>
									{#snippet child({ props })}
										<span
											{...props}
											data-shell-status-indicator={indicator.id}
											aria-label={indicator.label}
											class="flex h-5 min-w-11 items-center justify-center gap-1 font-mono text-[9px] leading-none text-sidebar-foreground/70"
										>
											<span class="size-1.5 shrink-0 rounded-full {indicator.tone}"></span>
											<span>{indicator.value}</span>
										</span>
									{/snippet}
								</Tooltip.Trigger>
								<Tooltip.Content side="right" align="center">{indicator.label}</Tooltip.Content>
							</Tooltip.Root>
						{/each}
					</div>
					{#if accessState.session?.local === false}
						<button
							type="button"
							class="grid size-9 place-items-center rounded-sm text-sidebar-foreground/55 hover:bg-sidebar-accent hover:text-sidebar-accent-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-none"
							aria-label="Sign out"
							title="Sign out"
							onclick={() => void controlClient.signOut()}
						>
							<LogOutIcon class="size-4" strokeWidth={1.75} />
						</button>
					{/if}
					<button
						type="button"
						class="grid size-9 place-items-center rounded-sm text-sidebar-foreground/55 hover:bg-sidebar-accent hover:text-sidebar-accent-foreground focus-visible:ring-2 focus-visible:ring-sidebar-ring focus-visible:outline-none"
						aria-label={appearance.effectiveTheme === 'dark'
							? 'Switch to light theme'
							: 'Switch to dark theme'}
						title={appearance.effectiveTheme === 'dark'
							? 'Switch to light theme'
							: 'Switch to dark theme'}
						onclick={toggleTheme}
					>
						{#if appearance.effectiveTheme === 'dark'}
							<SunIcon class="size-4" strokeWidth={1.75} />
						{:else}
							<MoonIcon class="size-4" strokeWidth={1.75} />
						{/if}
					</button>
				</div>
			</aside>

			<div
				class="flex min-h-svh min-w-0 flex-col {mobileCameraDetailActive
					? 'pb-0'
					: mobileFocusedActionActive
						? 'pb-[68px]'
						: 'pb-[78px]'} md:h-svh md:min-h-0 md:pb-0"
			>
				{#if !cameraDiagnosisActive}
					{#if settingsActive && page.url.hash.length === 0}
						<MobileSettingsHeader title="More" />
					{/if}
					<header
						data-shell-context
						class="h-[50px] shrink-0 items-center gap-3 border-b border-border bg-background px-4 md:h-[52px] {settingsActive ||
						healthOverviewActive ||
						mobileCameraWizardActive ||
						mobileCameraDetailActive
							? 'hidden'
							: 'flex'} {primaryViewActive ? 'md:hidden' : 'md:flex'}"
					>
						<a href={resolve('/')} class="font-semibold md:hidden" aria-label="KeepPeek home">
							KeepPeek
						</a>
						<span class="hidden text-sm font-semibold md:inline">{currentRoute.label}</span>
						<span class="ml-auto flex items-center gap-2 font-mono text-xs text-muted-foreground">
							<span class="size-1.5 rounded-full bg-healthy"></span>
							{accessState.session?.local ? 'Local' : 'Remote'} · {accessState.session?.role ===
							'administrator'
								? 'Administrator'
								: 'User'}
						</span>
						{#if accessState.session?.local === false}
							<button
								type="button"
								class="grid size-8 place-items-center rounded-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none md:hidden"
								aria-label="Sign out"
								title="Sign out"
								onclick={() => void controlClient.signOut()}
							>
								<LogOutIcon class="size-4" strokeWidth={1.75} />
							</button>
						{/if}
						<button
							type="button"
							class="grid size-8 place-items-center rounded-sm text-muted-foreground hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none md:hidden"
							aria-label={appearance.effectiveTheme === 'dark'
								? 'Switch to light theme'
								: 'Switch to dark theme'}
							title={appearance.effectiveTheme === 'dark'
								? 'Switch to light theme'
								: 'Switch to dark theme'}
							onclick={toggleTheme}
						>
							{#if appearance.effectiveTheme === 'dark'}
								<SunIcon class="size-4" strokeWidth={1.75} />
							{:else}
								<MoonIcon class="size-4" strokeWidth={1.75} />
							{/if}
						</button>
					</header>
				{/if}

				<main
					data-shell-main
					class="[container-type:size] min-h-0 flex-1 [container-name:view-shell] {dashboardActive ||
					page.url.pathname === '/viewer'
						? 'relative'
						: ''} {primaryViewActive ? 'overflow-hidden' : 'overflow-auto'} {page.url.pathname ===
					'/'
						? 'live-surface'
						: 'workspace-surface'}"
				>
					{@render children()}
				</main>
			</div>

			{#if !mobileRouteOwnsBottom}
				<MobileNavigation pathname={page.url.pathname} {administrator} />
			{/if}
		</div>
	</Tooltip.Provider>

	<KeyboardOverlay
		mode={keyboardOverlay}
		pathname={page.url.pathname}
		onclose={() => (keyboardOverlay = null)}
		onnavigate={navigateFromKeyboard}
	/>

	{#if navigationChordPending}
		<div
			data-keyboard-navigation-chord
			class="fixed bottom-12 left-1/2 z-[70] flex -translate-x-1/2 items-center gap-2 rounded-md border border-hairline-strong bg-raised px-3 py-2 shadow-lg"
			role="status"
			aria-live="polite"
		>
			<kbd
				class="rounded-sm border border-primary bg-primary px-1.5 py-1 font-mono text-2xs text-on-primary"
				>G</kbd
			>
			<span class="text-xs text-text-muted">Go to</span>
			{#each keyboardDestinations as destination (destination.key)}
				<span class="flex items-center gap-1">
					<kbd
						class="rounded-sm border border-hairline-strong bg-surface px-1.5 py-1 font-mono text-2xs uppercase"
						>{destination.key}</kbd
					>
					<span class="hidden text-2xs text-text-faint md:inline">{destination.label}</span>
				</span>
			{/each}
		</div>
	{/if}
{:else}
	<RemoteSignIn
		state={accessState}
		onsignin={(accessKey) => controlClient.signIn(accessKey)}
		onretry={() => controlClient.checkAccess()}
	/>
{/if}
