<script lang="ts">
	import { tick } from 'svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import type { BrowserLogEntry, LogLevel, ServerLogEntry } from '$lib/types';
	import DownloadIcon from '@lucide/svelte/icons/download';
	import PauseIcon from '@lucide/svelte/icons/pause';
	import PlayIcon from '@lucide/svelte/icons/play';
	import Trash2Icon from '@lucide/svelte/icons/trash-2';
	import WrapTextIcon from '@lucide/svelte/icons/wrap-text';

	type LogEntry = ServerLogEntry | BrowserLogEntry;
	type Props = {
		entries: LogEntry[];
		connection?: string;
		skipped?: number;
		onclear: () => void;
		ondownload: () => void;
		downloading?: boolean;
	};

	let {
		entries,
		connection,
		skipped = 0,
		onclear,
		ondownload,
		downloading = false
	}: Props = $props();
	let displayedEntries = $state.raw<LogEntry[]>([]);
	let paused = $state(false);
	let unread = $state(0);
	let follow = $state(true);
	let wrap = $state(false);
	let targetFilter = $state('');
	let textFilter = $state('');
	let viewport = $state<HTMLDivElement>();
	let levels = $state<Record<LogLevel, boolean>>({
		trace: true,
		debug: true,
		info: true,
		warn: true,
		error: true
	});

	const levelOptions: { value: LogLevel; label: string }[] = [
		{ value: 'trace', label: 'Trace' },
		{ value: 'debug', label: 'Debug' },
		{ value: 'info', label: 'Info' },
		{ value: 'warn', label: 'Warn' },
		{ value: 'error', label: 'Error' }
	];
	const timeFormatter = new Intl.DateTimeFormat(undefined, {
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit',
		fractionalSecondDigits: 3,
		hour12: false
	});

	let visibleEntries = $derived.by(() => {
		const target = targetFilter.trim().toLocaleLowerCase();
		const text = textFilter.trim().toLocaleLowerCase();
		return displayedEntries.filter((entry) => {
			if (!levels[entry.level]) return false;
			if (target && !entry.target.toLocaleLowerCase().includes(target)) return false;
			if (!text) return true;
			return `${entry.message} ${entry.target} ${JSON.stringify(entry.fields)}`
				.toLocaleLowerCase()
				.includes(text);
		});
	});

	$effect(() => {
		const latestEntries = entries;
		if (paused) {
			const latestDisplayed = displayedEntries.at(-1)?.sequence ?? 0;
			unread = latestEntries.filter((entry) => entry.sequence > latestDisplayed).length;
			return;
		}
		displayedEntries = latestEntries;
		unread = 0;
	});

	$effect(() => {
		if (visibleEntries.length === 0 || !follow || paused || !viewport) return;
		let cancelled = false;
		void tick().then(() => {
			if (!cancelled && viewport) viewport.scrollTop = viewport.scrollHeight;
		});
		return () => {
			cancelled = true;
		};
	});

	function togglePaused(): void {
		paused = !paused;
		if (!paused) {
			displayedEntries = entries;
			unread = 0;
		}
	}

	function clearView(): void {
		displayedEntries = [];
		unread = 0;
		onclear();
	}

	function levelClass(level: LogLevel): string {
		switch (level) {
			case 'error':
				return 'text-red-300 bg-red-500/10 border-red-500/25';
			case 'warn':
				return 'text-amber-300 bg-amber-500/10 border-amber-500/25';
			case 'info':
				return 'text-cyan-300 bg-cyan-500/10 border-cyan-500/25';
			case 'debug':
				return 'text-emerald-300 bg-emerald-500/10 border-emerald-500/25';
			default:
				return 'text-white/45 bg-white/5 border-white/10';
		}
	}

	function hasDetails(entry: LogEntry): boolean {
		return Object.keys(entry.fields).length > 0 || 'stack' in entry || Boolean(entry.file);
	}
</script>

<div class="space-y-3">
	<div class="flex flex-wrap items-end gap-2">
		<fieldset class="flex min-h-9 flex-wrap items-center gap-x-3 gap-y-1 rounded-md border px-3">
			<legend class="sr-only">Log levels</legend>
			{#each levelOptions as level (level.value)}
				<label class="flex items-center gap-1.5 text-xs font-medium capitalize">
					<input
						type="checkbox"
						class="size-3.5 accent-primary"
						bind:checked={levels[level.value]}
					/>
					{level.label}
				</label>
			{/each}
		</fieldset>
		<label class="grid min-w-36 flex-1 gap-1 text-xs font-medium" for="log-target-filter">
			<span class="sr-only">Target filter</span>
			<Input id="log-target-filter" bind:value={targetFilter} placeholder="Target" />
		</label>
		<label class="grid min-w-44 flex-[1.5] gap-1 text-xs font-medium" for="log-text-filter">
			<span class="sr-only">Text filter</span>
			<Input id="log-text-filter" bind:value={textFilter} placeholder="Search messages" />
		</label>
		<div class="flex items-center gap-1">
			<Button
				variant="outline"
				size="icon"
				onclick={togglePaused}
				aria-label={paused ? `Resume${unread ? ` (${unread} unread)` : ''}` : 'Pause'}
				title={paused ? 'Resume log updates' : 'Pause log updates'}
			>
				{#if paused}<PlayIcon />{:else}<PauseIcon />{/if}
			</Button>
			<Button
				variant={follow ? 'secondary' : 'outline'}
				size="icon"
				onclick={() => (follow = !follow)}
				aria-label="Follow latest logs"
				aria-pressed={follow}
				title="Follow latest logs"
			>
				<span class="font-mono text-xs">↓</span>
			</Button>
			<Button
				variant={wrap ? 'secondary' : 'outline'}
				size="icon"
				onclick={() => (wrap = !wrap)}
				aria-label="Wrap log lines"
				aria-pressed={wrap}
				title="Wrap log lines"
			>
				<WrapTextIcon />
			</Button>
			<Button
				variant="outline"
				size="icon"
				onclick={clearView}
				aria-label="Clear log view"
				title="Clear log view"
			>
				<Trash2Icon />
			</Button>
			<Button
				variant="outline"
				size="icon"
				onclick={ondownload}
				disabled={downloading}
				aria-label="Download bug report"
				title="Download bug report"
			>
				<DownloadIcon class={downloading ? 'animate-pulse' : undefined} />
			</Button>
		</div>
	</div>

	<div
		class="flex min-h-5 flex-wrap items-center gap-2 text-[11px] text-muted-foreground"
		role="status"
	>
		{#if connection}
			<span class="capitalize">{connection}</span>
		{/if}
		<span>{visibleEntries.length} shown</span>
		{#if paused}<span>{unread} unread</span>{/if}
		{#if skipped > 0}<span class="text-amber-400">{skipped} skipped</span>{/if}
	</div>

	<div
		bind:this={viewport}
		class="h-[clamp(22rem,62vh,48rem)] overflow-auto border-y border-white/10 bg-[#0b0d0f] text-white"
		role="log"
		aria-label="Log entries"
		aria-live={paused ? 'off' : 'polite'}
	>
		{#if visibleEntries.length === 0}
			<div class="grid h-full min-h-48 place-items-center px-4 text-sm text-white/40">
				No logs match the current filters.
			</div>
		{:else}
			<div class={wrap ? 'min-w-0' : 'min-w-[52rem]'}>
				{#each visibleEntries as entry (`${entry.target}-${entry.sequence}`)}
					<div
						class="grid grid-cols-[7.5rem_4.5rem_minmax(11rem,17rem)_1fr] border-b border-white/5 text-xs hover:bg-white/[0.035]"
					>
						<time
							class="px-3 py-2 font-mono text-[11px] text-white/45 tabular-nums"
							datetime={new Date(entry.timestamp_ms).toISOString()}
							title={new Date(entry.timestamp_ms).toLocaleString()}
						>
							{timeFormatter.format(entry.timestamp_ms)}
						</time>
						<div class="px-2 py-1.5">
							<span
								class="inline-flex rounded border px-1.5 py-0.5 font-mono text-[10px] uppercase {levelClass(
									entry.level
								)}"
							>
								{entry.level}
							</span>
						</div>
						<div
							class="truncate px-2 py-2 font-mono text-[11px] text-white/50"
							title={entry.target}
						>
							{entry.target}
						</div>
						<div class="min-w-0 px-3 py-2">
							<p
								class="font-mono leading-5 {wrap ? 'break-words whitespace-pre-wrap' : 'truncate'}"
							>
								{entry.message}
							</p>
							{#if hasDetails(entry)}
								<details class="mt-1 text-[11px] text-white/45">
									<summary class="w-fit cursor-pointer select-none hover:text-white/70"
										>Details</summary
									>
									<pre
										class="mt-1 overflow-x-auto font-mono break-words whitespace-pre-wrap">{JSON.stringify(
											{
												fields: entry.fields,
												file: entry.file,
												line: entry.line,
												...('stack' in entry ? { stack: entry.stack } : {})
											},
											null,
											2
										)}</pre>
								</details>
							{/if}
						</div>
					</div>
				{/each}
			</div>
		{/if}
	</div>
</div>
