<script lang="ts">
	import ArrowDownIcon from '@lucide/svelte/icons/arrow-down';

	type Props = {
		label: string;
		visibleClass: string;
		forceVisible?: boolean;
		onclick: (event: MouseEvent) => void;
		onpointerdown: (event: PointerEvent) => void;
		onpointermove: (event: PointerEvent) => void;
		onpointerup: (event: PointerEvent) => void;
		onpointercancel: (event: PointerEvent) => void;
		onlostpointercapture: (event: PointerEvent) => void;
		onkeydown: (event: KeyboardEvent) => void;
	};

	let {
		label,
		visibleClass,
		forceVisible = false,
		onclick,
		onpointerdown,
		onpointermove,
		onpointerup,
		onpointercancel,
		onlostpointercapture,
		onkeydown
	}: Props = $props();
</script>

<div
	data-peek-rewind-control
	class="pointer-events-none absolute top-1/2 left-1/2 z-40 mt-px -translate-x-1/2 -translate-y-1/2 flex-col items-center gap-2 transition-opacity {forceVisible
		? 'flex opacity-100'
		: `opacity-0 group-focus-within:opacity-100 group-hover:opacity-100 ${visibleClass}`}"
>
	<button
		type="button"
		class="pointer-events-auto grid size-14 touch-none place-items-center rounded-full border border-hairline-strong bg-video/80 text-white focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
		aria-label={`Rewind ${label}`}
		title="Drag down to go back"
		{onclick}
		{onpointerdown}
		{onpointermove}
		{onpointerup}
		{onpointercancel}
		{onlostpointercapture}
		{onkeydown}
	>
		<ArrowDownIcon class="size-[22px]" strokeWidth={1.75} />
	</button>
	<span class="font-mono text-2xs leading-3 tracking-caps text-white/70 uppercase">
		Drag down to go back
	</span>
</div>
