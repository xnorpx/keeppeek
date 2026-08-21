<script lang="ts">
	type Props = {
		label: string;
		elapsedMs: number;
		lateAfterMs?: number;
		class?: string;
	};

	let { label, elapsedMs, lateAfterMs = 5_000, class: className = '' }: Props = $props();
	let late = $derived(elapsedMs >= lateAfterMs);
</script>

<div
	data-first-frame-state={late ? 'late' : 'waiting'}
	data-first-frame-elapsed-ms={Math.round(elapsedMs)}
	class="flex flex-col justify-between bg-video p-3 text-[#E8E9EA] {className}"
	role="status"
>
	<span
		class="inline-flex h-[22px] items-center gap-1.5 self-start rounded-xs bg-[#0C0D0FB8] px-2 font-mono text-2xs leading-3 tracking-[0.08em]"
	>
		<span class="size-[5px] rounded-full bg-[#E8A33D]"></span>
		<span class="tracking-[0.08em]">{late ? 'DEGRADED' : 'CONNECTING'}</span>
	</span>
	<div class="flex flex-col items-center gap-1.5">
		<div class="flex h-[3px] w-[180px] overflow-hidden rounded-full bg-[#FFFFFF2E]">
			<div class="w-[72px] bg-[#E8A33D]"></div>
		</div>
		<p class="font-mono text-2xs leading-3 tracking-caps text-white/70 uppercase">
			{late
				? `No keyframe after ${(elapsedMs / 1_000).toFixed(1)}s`
				: `Negotiated · waiting for a keyframe · ${(elapsedMs / 1_000).toFixed(1)}s`}
		</p>
	</div>
	<p class="text-md leading-[18px] font-semibold">{label}</p>
</div>
