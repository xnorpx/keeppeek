<script lang="ts">
	type Props = {
		label: string;
		elapsedMs: number;
		frameUrl?: string | null;
		class?: string;
	};

	let { label, elapsedMs, frameUrl = null, class: className = '' }: Props = $props();
</script>

<div
	data-first-frame-state="waiting"
	data-first-frame-elapsed-ms={Math.round(elapsedMs)}
	data-cached-frame={frameUrl ? 'true' : undefined}
	class="relative flex flex-col justify-between overflow-hidden {frameUrl
		? ''
		: 'bg-video'} p-3 text-[#E8E9EA] {className}"
	role="status"
>
	{#if frameUrl}
		<img
			data-peek-cached-frame
			src={frameUrl}
			alt=""
			class="pointer-events-none absolute inset-0 size-full object-cover"
		/>
		<div class="pointer-events-none absolute inset-0 bg-black/20"></div>
	{/if}
	<span
		class="relative z-10 inline-flex h-[22px] items-center gap-1.5 self-start rounded-xs bg-[#0C0D0FB8] px-2 font-mono text-2xs leading-3 tracking-[0.08em]"
	>
		<span class="size-[5px] rounded-full {frameUrl ? 'bg-amber-400' : 'bg-text-muted'}"></span>
		<span class="tracking-[0.08em]">{frameUrl ? 'RESTORING' : 'CONNECTING'}</span>
	</span>
	<div class="relative z-10 flex flex-col items-center gap-1.5">
		<div class="flex h-[3px] w-[180px] overflow-hidden rounded-full bg-[#FFFFFF2E]">
			<div class="w-[72px] {frameUrl ? 'bg-amber-400' : 'bg-text-muted'}"></div>
		</div>
		<p class="font-mono text-2xs leading-3 tracking-caps text-white/70 uppercase">
			{frameUrl
				? `Showing last frame · waiting for live video · ${(elapsedMs / 1_000).toFixed(1)}s`
				: `Negotiated · waiting for a keyframe · ${(elapsedMs / 1_000).toFixed(1)}s`}
		</p>
	</div>
	<p class="relative z-10 text-md leading-[18px] font-semibold">{label}</p>
</div>
