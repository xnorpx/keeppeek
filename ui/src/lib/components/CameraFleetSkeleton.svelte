<script lang="ts">
	type Props = {
		cameraCount: number;
		compact?: boolean;
		statusLabel?: string;
		class?: string;
	};

	let { cameraCount, compact = false, statusLabel, class: className = '' }: Props = $props();
	const compactRows = [
		[118, 64, 72, 44],
		[96, 64, 72, 38],
		[110, 64, 72, 44]
	] as const;
	const fullRows = [0, 1, 2, 3] as const;
	let resolvedStatus = $derived(
		statusLabel ??
			`Reading health evidence · ${cameraCount} ${cameraCount === 1 ? 'camera' : 'cameras'} in inventory`
	);
</script>

{#if compact}
	<div data-fleet-skeleton class="flex flex-col bg-raised {className}">
		<div
			class="flex h-[30px] shrink-0 items-center gap-3.5 border-b border-hairline px-[18px] font-mono text-2xs leading-3 tracking-caps text-text-faint uppercase"
		>
			<span class="w-[118px] shrink-0">Camera</span>
			<span class="w-[78px] shrink-0">Transport</span>
			<span class="w-[88px] shrink-0">Recording</span>
			<span class="min-w-0 flex-1">GB/day</span>
		</div>
		{#each compactRows as row, rowIndex (rowIndex)}
			<div class="flex h-14 shrink-0 items-center gap-3.5 border-b border-hairline px-[18px]">
				{#each row as width, columnIndex (`${rowIndex}-${columnIndex}`)}
					<span
						class="h-[11px] shrink-0 rounded-xs {columnIndex === 0
							? 'bg-hairline-strong'
							: 'bg-hairline'}"
						style:width={`${width}px`}
					></span>
				{/each}
			</div>
		{/each}
		<div
			class="flex h-10 shrink-0 items-center px-[18px] font-mono text-2xs leading-3 tracking-caps text-text-faint uppercase"
			role="status"
		>
			{resolvedStatus}
		</div>
	</div>
{:else}
	<div data-fleet-skeleton class="min-w-0 overflow-x-auto border-y border-hairline {className}">
		<div class="min-w-[1314px] bg-raised">
			<div
				class="grid h-[34px] grid-cols-[32px_20px_270px_140px_230px_150px_140px_120px_152px_60px] items-center border-b border-hairline-strong font-mono text-2xs tracking-caps text-text-faint"
			>
				<span></span><span></span><span>CAMERA</span><span>TRANSPORT</span><span>STREAMS</span><span
					>RECORDING</span
				><span>THROUGHPUT</span><span>GB / DAY</span><span>LAST EVENT</span><span></span>
			</div>
			{#each fullRows as row (row)}
				<div
					class="grid h-14 grid-cols-[32px_20px_270px_140px_230px_150px_140px_120px_152px_60px] items-center border-b border-hairline"
				>
					<span></span>
					<span class="size-2 rounded-full bg-hairline-strong"></span>
					<span class="h-3 w-40 rounded-xs bg-hairline-strong"></span>
					<span class="h-3 w-24 rounded-xs bg-hairline"></span>
					<span class="h-3 w-44 rounded-xs bg-hairline"></span>
					<span class="h-3 w-24 rounded-xs bg-hairline"></span>
					<span class="h-3 w-20 rounded-xs bg-hairline"></span>
					<span class="h-3 w-16 rounded-xs bg-hairline"></span>
					<span class="h-3 w-24 rounded-xs bg-hairline"></span>
					<span></span>
				</div>
			{/each}
			<div
				class="flex h-10 items-center px-3 font-mono text-2xs tracking-caps text-text-faint"
				role="status"
			>
				{resolvedStatus}
			</div>
		</div>
	</div>
{/if}
