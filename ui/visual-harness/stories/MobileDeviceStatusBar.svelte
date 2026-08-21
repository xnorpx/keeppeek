<script lang="ts">
	import BatteryFullIcon from '@lucide/svelte/icons/battery-full';
	import SignalIcon from '@lucide/svelte/icons/signal';
	import WifiIcon from '@lucide/svelte/icons/wifi';
</script>

<div
	data-mobile-status-bar
	class="grid h-[62px] shrink-0 grid-cols-[93px_93px] items-center justify-between px-6 text-white"
>
	<span class="text-center font-[system-ui] text-[17px] leading-[22px] font-semibold">9:41</span>
	<div class="flex items-center justify-center gap-1.5" aria-hidden="true">
		<SignalIcon class="size-[18px]" strokeWidth={2.25} />
		<WifiIcon class="size-[18px]" strokeWidth={2.25} />
		<BatteryFullIcon class="h-[18px] w-[25px]" strokeWidth={1.75} />
	</div>
</div>
