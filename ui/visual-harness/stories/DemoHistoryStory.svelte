<script lang="ts">
	import PlayIcon from '@lucide/svelte/icons/play';
	import Board31HistoryStory from './Board31HistoryStory.svelte';

	const timeFormatter = new Intl.DateTimeFormat('en-GB', {
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit',
		hour12: false,
		timeZone: 'UTC'
	});

	let landedAtMs = $state<number | null>(null);
	let landedTimeLabel = $derived(
		landedAtMs === null ? '' : timeFormatter.format(new Date(landedAtMs))
	);

	function openKeep(_cameraId: string): void {
		landedAtMs = Date.parse('2026-08-18T06:37:23Z');
	}
</script>

{#if landedAtMs === null}
	<Board31HistoryStory state="focused" onhistory={openKeep} />
{:else}
	<main
		data-paper-scenario="peek.desktop.history-keep"
		data-demo-landed-in-keep
		class="flex h-[262px] w-[464px] flex-col overflow-hidden bg-ground text-foreground [font-synthesis:none]"
	>
		<header class="flex h-8 shrink-0 items-center border-b border-hairline bg-surface px-3">
			<span class="font-mono text-[10px] font-semibold tracking-[0.14em] text-primary">KEEP</span>
			<span class="mx-2.5 h-3 w-px bg-hairline"></span>
			<strong class="text-[11px] leading-4 font-semibold">Front Door</strong>
			<span class="flex-1"></span>
			<span class="font-mono text-[9px] leading-3 text-text-muted">Tue 18 Aug 2026</span>
		</header>
		<section class="relative min-h-0 flex-1 bg-video" aria-label="Recorded Front Door video">
			<div class="absolute top-2 left-2.5 font-mono text-[9px] leading-3 text-white/80">
				2026-08-18 {landedTimeLabel}
			</div>
			<div
				class="absolute top-2 right-2.5 font-mono text-[8px] leading-3 tracking-[0.12em] text-white/55"
			>
				MAIN · 25FPS
			</div>
			<div class="absolute inset-0 flex items-center justify-center">
				<span
					class="flex size-9 items-center justify-center rounded-full border border-white/25 bg-black/35 text-white"
				>
					<PlayIcon class="ml-px size-3.5" fill="currentColor" />
				</span>
			</div>
			<div class="absolute right-2.5 bottom-2 left-2.5 flex items-center gap-2">
				<span class="font-mono text-[8px] leading-3 text-white/60">−2M</span>
				<span class="relative h-0.5 flex-1 bg-white/20">
					<span class="absolute inset-y-[-3px] left-[68%] w-px bg-primary"></span>
				</span>
				<span class="font-mono text-[8px] leading-3 text-white">{landedTimeLabel}</span>
			</div>
		</section>
		<footer
			class="flex h-9 shrink-0 items-center gap-2 border-t border-hairline bg-surface px-3"
			aria-label="Keep timeline"
		>
			<span class="font-mono text-[8px] leading-3 text-text-muted">06:35</span>
			<span class="relative h-2 flex-1 bg-raised">
				<span class="absolute inset-y-0 right-[3%] left-[8%] bg-primary/35"></span>
				<span class="absolute inset-y-[-3px] left-[68%] w-px bg-primary"></span>
			</span>
			<span class="font-mono text-[8px] leading-3 text-text-muted">06:38</span>
		</footer>
	</main>
{/if}
