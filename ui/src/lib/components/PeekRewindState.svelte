<script lang="ts">
	type Props = {
		rewindSeconds: number;
		targetTimeLabel: string;
		markerPercent: number;
		class?: string;
	};

	let { rewindSeconds, targetTimeLabel, markerPercent, class: className = '' }: Props = $props();
</script>

<div
	data-peek-rewind
	data-peek-rewind-seconds={rewindSeconds}
	class="pointer-events-none flex flex-col justify-between rounded-lg border-2 border-primary bg-video p-3 text-white {className}"
	role="status"
>
	<div class="flex items-center justify-between gap-2 font-mono text-2xs leading-3">
		<span
			class="inline-flex h-[22px] items-center rounded-xs bg-primary/90 px-2 tracking-[0.08em] text-on-primary"
		>
			REWINDING
		</span>
		<span class="text-white/80">{targetTimeLabel}</span>
	</div>
	<div class="flex flex-col items-center gap-1">
		<p class="text-3xl leading-[42px] font-bold tracking-tight">−{rewindSeconds}s</p>
		<p class="font-mono text-2xs leading-3 tracking-caps text-white/70 uppercase">
			Release to open here · Esc returns to live
		</p>
	</div>
	<div class="flex flex-col gap-1.5">
		<div class="relative h-2.5 overflow-hidden rounded-xs bg-ground/75">
			<div class="absolute inset-0 bg-availability"></div>
			<span
				data-rewind-marker
				class="absolute inset-y-0 w-0.5 bg-primary"
				style:left={`${Math.max(0, Math.min(100, markerPercent))}%`}
			></span>
		</div>
		<div
			class="flex justify-between font-mono text-2xs leading-3 tracking-caps text-white/60 uppercase"
		>
			<span>−2m</span>
			<span>Now</span>
		</div>
	</div>
</div>
