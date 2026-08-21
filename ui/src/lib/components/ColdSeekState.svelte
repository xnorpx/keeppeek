<script lang="ts">
	type Props = {
		timestampLabel: string;
		timestampLines?: readonly [string, string];
		elapsedMs: number;
		activityLabel?: string;
		detail?: string;
		overlay?: boolean;
		class?: string;
	};

	let {
		timestampLabel,
		timestampLines,
		elapsedMs,
		activityLabel = 'Opening indexed recording',
		detail = 'The frame you were on stays until the new one arrives',
		overlay = false,
		class: className = ''
	}: Props = $props();
</script>

<div
	data-cold-seek
	data-cold-seek-elapsed-ms={Math.round(elapsedMs)}
	class="flex flex-col items-center justify-center gap-2 p-3 text-center {overlay
		? 'bg-black/55'
		: 'bg-video'} {className}"
	role="status"
>
	<p
		class="text-2xl leading-8 font-bold tracking-tight text-white/70 {timestampLines
			? 'h-8 w-[218px] shrink-0 text-left'
			: ''}"
		aria-label={timestampLabel}
	>
		{#if timestampLines}
			<span class="block">{timestampLines[0]}</span><span class="block">{timestampLines[1]}</span>
		{:else}
			{timestampLabel}
		{/if}
	</p>
	<p class="font-mono text-2xs leading-3 tracking-caps text-[#E8A33D] uppercase">
		{activityLabel} · {(elapsedMs / 1_000).toFixed(1)}s
	</p>
	<p class="font-mono text-2xs leading-3 tracking-caps text-white/50 uppercase">{detail}</p>
</div>
