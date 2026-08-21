<script lang="ts">
	import { resolve } from '$app/paths';
	import RotateCcwIcon from '@lucide/svelte/icons/rotate-ccw';

	type Props = {
		cameraLabel: string;
		timestampMs: number;
		timeLabel: string;
		class?: string;
	};

	let { cameraLabel, timestampMs, timeLabel, class: className = '' }: Props = $props();
</script>

<a
	data-peek-entry
	data-peek-entry-timestamp={timestampMs}
	href={resolve('/')}
	class="flex h-[38px] items-center gap-2 border-b border-hairline bg-primary/10 px-3 text-xs-plus leading-4 text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {className}"
>
	<RotateCcwIcon class="size-[13px] shrink-0 text-primary-soft" strokeWidth={1.75} />
	<span>From Peek · {cameraLabel} · {timeLabel}</span>
</a>
