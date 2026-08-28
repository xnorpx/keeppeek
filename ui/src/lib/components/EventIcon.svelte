<script lang="ts">
	import { eventIconKey } from '$lib/event-presentation';
	import type { EventIconKey } from '$lib/types';
	import ActivityIcon from '@lucide/svelte/icons/activity';
	import AudioLinesIcon from '@lucide/svelte/icons/audio-lines';
	import BellRingIcon from '@lucide/svelte/icons/bell-ring';
	import CarFrontIcon from '@lucide/svelte/icons/car-front';
	import ImagesIcon from '@lucide/svelte/icons/images';
	import PackageIcon from '@lucide/svelte/icons/package';
	import PawPrintIcon from '@lucide/svelte/icons/paw-print';
	import ScanLineIcon from '@lucide/svelte/icons/scan-line';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';
	import UserRoundIcon from '@lucide/svelte/icons/user-round';
	import type { Component } from 'svelte';

	type Props = {
		iconKey?: string | null;
		eventType: string;
		class?: string;
	};

	let { iconKey, eventType, class: className = 'size-4' }: Props = $props();
	const icons: Record<EventIconKey, Component<{ class?: string; 'aria-hidden'?: boolean }>> = {
		event: ActivityIcon,
		person: UserRoundIcon,
		vehicle: CarFrontIcon,
		animal: PawPrintIcon,
		package: PackageIcon,
		motion: ScanLineIcon,
		doorbell: BellRingIcon,
		sound: AudioLinesIcon,
		story: ImagesIcon,
		alert: TriangleAlertIcon
	};
	let Icon = $derived(icons[eventIconKey(iconKey, eventType)]);
</script>

<Icon class={className} aria-hidden={true} />
