<script lang="ts">
	import { resolve } from '$app/paths';
	import { useControlClient } from '$lib/control-context';
	import { keyboardDestinations, type KeyboardOverlayMode } from '$lib/keyboard-shortcuts';
	import type { CameraListItem } from '$lib/types';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import SearchIcon from '@lucide/svelte/icons/search';
	import XIcon from '@lucide/svelte/icons/x';

	type CommandEntry = {
		id: string;
		label: string;
		detail: string;
		href: string;
		keywords: string;
		type: 'camera' | 'destination' | 'setting';
	};

	type Props = {
		mode: KeyboardOverlayMode;
		pathname: string;
		onclose: () => void;
		onnavigate: (href: string) => void;
	};

	let { mode, pathname, onclose, onnavigate }: Props = $props();
	const controlClient = useControlClient();
	let dialog = $state<HTMLDialogElement | null>(null);
	let searchInput = $state<HTMLInputElement | null>(null);
	let query = $state('');
	let cameras = $state.raw<CameraListItem[]>([]);
	let loading = $state(false);
	let loadError = $state<string | null>(null);
	let selectedIndex = $state(0);

	const fixedCommands: CommandEntry[] = [
		...keyboardDestinations.map((destination) => ({
			id: `destination-${destination.key}`,
			label: destination.label,
			detail: 'Destination',
			href: destination.href,
			keywords: destination.label.toLowerCase(),
			type: 'destination' as const
		})),
		...[
			['Storage & retention', 'storage'],
			['Camera defaults', 'camera-defaults'],
			['Event sources', 'event-sources'],
			['Groups', 'groups'],
			['Access & roles', 'access'],
			['Integrations', 'integrations'],
			['Notifications', 'notifications'],
			['Appearance & system', 'appearance']
		].map(([label, section]) => ({
			id: `setting-${section}`,
			label,
			detail: 'Setting',
			href: `${resolve('/settings')}#${section}`,
			keywords: `${label} settings`.toLowerCase(),
			type: 'setting' as const
		}))
	];

	let cameraCommands = $derived(
		cameras.map((camera): CommandEntry => ({
			id: `camera-${camera.id}`,
			label: camera.name ?? camera.id,
			detail: camera.ip,
			href: `${resolve('/camera')}?camera=${encodeURIComponent(camera.id)}`,
			keywords: `${camera.name ?? ''} ${camera.id} ${camera.ip}`.toLowerCase(),
			type: 'camera'
		}))
	);
	let filteredCommands = $derived.by(() => {
		const needle = query.trim().toLowerCase();
		const commands = [...cameraCommands, ...fixedCommands];
		return (
			needle ? commands.filter((command) => command.keywords.includes(needle)) : commands
		).slice(0, 10);
	});

	$effect(() => {
		const element = dialog;
		if (!element) return;
		if (mode !== null && !element.open) {
			element.showModal();
			queueMicrotask(() => searchInput?.focus());
		} else if (mode === null && element.open) {
			element.close();
		}
	});

	$effect(() => {
		if (mode !== 'commands') return;
		query = '';
		selectedIndex = 0;
		let active = true;
		loading = true;
		loadError = null;
		void controlClient
			.getCameras()
			.then(
				(value) => {
					if (active) cameras = value;
				},
				(cause: unknown) => {
					if (active) {
						loadError = cause instanceof Error ? cause.message : 'Camera search is unavailable.';
					}
				}
			)
			.finally(() => {
				if (active) loading = false;
			});
		return () => {
			active = false;
		};
	});

	$effect(() => {
		resetSelectedIndex(query, filteredCommands.length);
	});

	function resetSelectedIndex(_query: string, _commandCount: number): void {
		selectedIndex = 0;
	}

	function closeFromDialog(): void {
		if (mode !== null) onclose();
	}

	function cancelDialog(event: Event): void {
		event.preventDefault();
		onclose();
	}

	function closeFromBackdrop(event: MouseEvent): void {
		if (event.target === dialog) onclose();
	}

	function navigate(command: CommandEntry): void {
		onnavigate(command.href);
	}

	function handleCommandKeydown(event: KeyboardEvent): void {
		if (event.key === 'ArrowDown') {
			event.preventDefault();
			selectedIndex = filteredCommands.length ? (selectedIndex + 1) % filteredCommands.length : 0;
		}
		if (event.key === 'ArrowUp') {
			event.preventDefault();
			selectedIndex = filteredCommands.length
				? (selectedIndex - 1 + filteredCommands.length) % filteredCommands.length
				: 0;
		}
		if (event.key === 'Enter') {
			const selected = filteredCommands[selectedIndex];
			if (!selected) return;
			event.preventDefault();
			navigate(selected);
		}
	}

	const anywhereShortcuts = [
		{ keys: ['⌘K'], label: 'Find a camera or setting' },
		{ keys: ['G', 'P/K/E/C/H/S'], label: 'Go to a destination' },
		{ keys: ['?'], label: 'Open keyboard shortcuts' },
		{ keys: ['Esc'], label: 'Close the top-most dialog' }
	];
	let contextualShortcuts = $derived(
		pathname === '/'
			? [
					{ keys: ['←', '→', '↑', '↓'], label: 'Move focus across the camera grid' },
					{ keys: ['F'], label: 'Focus the selected camera or return to the grid' },
					{ keys: ['↓'], label: 'Rewind from the focused camera control' },
					{ keys: ['Enter'], label: 'Open the focused camera or selected rewind point' }
				]
			: pathname.startsWith('/keep')
				? [
						{ keys: ['J', 'K', 'L'], label: 'Shuttle backward, pause, or forward' },
						{ keys: ['←', '→'], label: 'Step one reported video frame' },
						{ keys: ['[', ']'], label: 'Set export range in and out' },
						{ keys: ['Space'], label: 'Play or pause without changing speed' },
						{ keys: ['Home'], label: 'Return to the live edge and follow' }
					]
				: pathname.startsWith('/settings')
					? [
							{ keys: ['/'], label: 'Focus search when this screen has one' },
							{ keys: ['⌘S'], label: 'Save the active settings draft' }
						]
					: [
							{ keys: ['/'], label: 'Focus search when this screen has one' },
							{ keys: ['↑', '↓'], label: 'Move the focused row or card' },
							{ keys: ['Enter'], label: 'Open the focused control or row' },
							{ keys: ['Space'], label: 'Toggle bulk selection when available' }
						]
	);
</script>

<dialog
	bind:this={dialog}
	class="m-auto max-h-[min(46rem,calc(100svh-2rem))] w-[min(44rem,calc(100vw-2rem))] overflow-hidden rounded-lg border border-hairline-strong bg-surface p-0 text-foreground shadow-2xl backdrop:bg-black/70"
	aria-labelledby={mode === 'commands' ? 'command-palette-heading' : 'keyboard-help-heading'}
	oncancel={cancelDialog}
	onclose={closeFromDialog}
	onclick={closeFromBackdrop}
>
	{#if mode === 'commands'}
		<section data-command-palette class="flex max-h-[min(42rem,calc(100svh-2rem))] flex-col">
			<header class="flex h-14 shrink-0 items-center gap-3 border-b border-hairline px-4">
				<SearchIcon class="size-4 shrink-0 text-primary-soft" />
				<h2 id="command-palette-heading" class="sr-only">Find a camera or setting</h2>
				<input
					bind:this={searchInput}
					bind:value={query}
					type="search"
					autocomplete="off"
					placeholder="Find a camera or setting"
					class="min-w-0 flex-1 bg-transparent text-sm outline-none placeholder:text-text-faint"
					onkeydown={handleCommandKeydown}
				/>
				<kbd
					class="rounded-sm border border-hairline-strong bg-raised px-1.5 py-1 font-mono text-2xs text-text-muted"
					>Esc</kbd
				>
			</header>
			<div class="min-h-24 overflow-y-auto p-2" role="listbox" aria-label="Command results">
				{#if loading && cameras.length === 0}
					<p class="px-3 py-4 text-sm text-text-muted">Loading cameras…</p>
				{:else if filteredCommands.length === 0}
					<p class="px-3 py-4 text-sm text-text-muted">No camera or setting matches “{query}”.</p>
				{:else}
					{#each filteredCommands as command, index (command.id)}
						<button
							type="button"
							role="option"
							aria-selected={index === selectedIndex}
							class="flex min-h-11 w-full items-center gap-3 rounded-sm px-3 text-left {index ===
							selectedIndex
								? 'bg-raised text-foreground'
								: 'text-text-muted hover:bg-raised/60'}"
							onmouseenter={() => (selectedIndex = index)}
							onclick={() => navigate(command)}
						>
							{#if command.type === 'camera'}
								<CameraIcon class="size-4 shrink-0 text-primary-soft" />
							{:else}
								<span class="size-4 shrink-0 rounded-sm border border-hairline-strong"></span>
							{/if}
							<span class="min-w-0 flex-1 truncate text-sm font-medium">{command.label}</span>
							<span class="shrink-0 font-mono text-2xs text-text-faint">{command.detail}</span>
						</button>
					{/each}
				{/if}
				{#if loadError}<p class="px-3 py-2 text-xs text-live-text">{loadError}</p>{/if}
			</div>
			<footer
				class="flex h-9 shrink-0 items-center gap-3 border-t border-hairline px-4 font-mono text-2xs text-text-faint"
			>
				<span>↑↓ select</span><span>Enter open</span>
			</footer>
		</section>
	{:else if mode === 'help'}
		<section data-keyboard-help class="flex max-h-[min(46rem,calc(100svh-2rem))] flex-col">
			<header class="flex shrink-0 items-start gap-4 border-b border-hairline px-5 py-4">
				<div class="min-w-0 flex-1">
					<p class="font-mono text-2xs tracking-caps text-primary-soft">KEYBOARD</p>
					<h2 id="keyboard-help-heading" class="mt-1 text-lg-plus font-semibold">
						Shortcuts and focus
					</h2>
					<p class="mt-1 text-sm text-text-muted">
						Typing in a field always wins over single-letter shortcuts.
					</p>
				</div>
				<button
					type="button"
					class="grid size-8 shrink-0 place-items-center rounded-sm text-text-muted hover:bg-raised hover:text-foreground"
					aria-label="Close keyboard shortcuts"
					onclick={onclose}
				>
					<XIcon class="size-4" />
				</button>
			</header>
			<div class="grid min-h-0 gap-5 overflow-y-auto p-5 md:grid-cols-2">
				{#each [{ title: 'Anywhere', items: anywhereShortcuts }, { title: 'This screen', items: contextualShortcuts }] as group (group.title)}
					<section>
						<h3 class="mb-2 text-sm font-semibold">{group.title}</h3>
						<div class="overflow-hidden rounded-md border border-hairline">
							{#each group.items as item, index (`${group.title}-${item.label}`)}
								<div
									class="flex min-h-12 items-center gap-3 px-3 {index > 0
										? 'border-t border-hairline'
										: ''}"
								>
									<div class="flex w-28 shrink-0 items-center gap-1">
										{#each item.keys as key (key)}
											<kbd
												class="rounded-sm border border-hairline-strong bg-raised px-1.5 py-1 font-mono text-2xs"
												>{key}</kbd
											>
										{/each}
									</div>
									<p class="text-sm leading-[18px] text-text-muted">{item.label}</p>
								</div>
							{/each}
						</div>
					</section>
				{/each}
			</div>
			<footer class="border-t border-hairline px-5 py-3 text-xs leading-5 text-text-faint">
				No shortcut is destructive. Gated actions remain gated, and every shortcut has a visible
				control.
			</footer>
		</section>
	{/if}
</dialog>
