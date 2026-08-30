<script lang="ts">
	import { onMount } from 'svelte';
	import HistoryIcon from '@lucide/svelte/icons/history';
	import Undo2Icon from '@lucide/svelte/icons/undo-2';

	type State = 'focused' | 'keep';
	type Props = {
		state: State;
		onhistory?: (cameraId: string) => void | Promise<void>;
	};

	let { state, onhistory = () => {} }: Props = $props();
	const cameraId = 'front-door';
	const filmstripCameras = ['Front Door', 'Driveway', 'Porch'] as const;

	onMount(() => {
		const root = document.documentElement;
		const previousTheme = root.dataset.theme;
		const wasDark = root.classList.contains('dark');
		root.classList.add('dark');
		root.dataset.theme = 'dark';
		return () => {
			root.classList.toggle('dark', wasDark);
			if (previousTheme === undefined) delete root.dataset.theme;
			else root.dataset.theme = previousTheme;
		};
	});
</script>

<main
	data-paper-scenario={state === 'focused'
		? 'peek.desktop.focus-history'
		: 'peek.desktop.history-keep'}
	class="h-[262px] w-[464px] overflow-hidden [font-synthesis:none]"
>
	{#if state === 'focused'}
		<section
			data-peek-focus-history
			aria-label="Front Door focus"
			class="flex size-full flex-col justify-between rounded-[var(--radius-lg)] border border-hairline bg-video p-3"
		>
			<div class="flex items-center justify-between">
				<div
					class="flex h-[22px] shrink-0 items-center rounded-[var(--radius-xs)] bg-[#0C0D0FB8] px-2"
				>
					<span class="font-mono text-2xs tracking-[0.08em] text-foreground">PEEK / FRONT DOOR</span
					>
				</div>
				<span class="font-mono text-2xs text-[#FFFFFFD1]">06:37:23</span>
			</div>
			<div class="flex h-[76px] w-full shrink-0 flex-col items-center justify-center gap-2">
				<button
					type="button"
					data-peek-history
					class="inline-flex h-[34px] items-center gap-[7px] rounded-[var(--radius-sm)] border border-hairline-strong bg-raised px-3 font-mono text-[10px] leading-3 font-semibold tracking-[0.12em] text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					onclick={() => void onhistory(cameraId)}
				>
					<HistoryIcon class="size-3.5" />
					HISTORY
				</button>
				<p class="text-xs leading-4 text-text-muted">Open Keep for this camera</p>
			</div>
			<aside
				data-focus-filmstrip
				class="flex h-10 shrink-0 gap-1 rounded-sm bg-[#0C0D0FB8] p-1"
				aria-label="Camera filmstrip"
			>
				{#each filmstripCameras as camera (camera)}
					<span
						class="flex min-w-0 flex-1 items-end rounded-xs border px-1.5 pb-1 text-2xs font-medium {camera ===
						'Front Door'
							? 'border-primary text-white'
							: 'border-white/10 text-white/55'}">{camera}</span
					>
				{/each}
			</aside>
		</section>
	{:else}
		<section
			data-history-keep
			class="flex size-full flex-col overflow-hidden rounded-[var(--radius-lg)] border border-hairline-strong bg-surface text-foreground"
			aria-label="Front Door history in Keep"
		>
			<header
				class="flex h-[38px] shrink-0 items-center gap-2 border-b border-hairline bg-[#B7410E24] px-3"
			>
				<Undo2Icon class="size-[13px] shrink-0 text-primary-soft" />
				<span class="text-[13px] leading-4 text-foreground">From Viewer · Front Door</span>
			</header>
			<div class="flex min-h-0 flex-1">
				<div class="flex min-w-0 flex-1 flex-col justify-end bg-video p-3">
					<span class="font-mono text-2xs text-white/60">MAIN · 3840×2160 · PAUSED AT 06:36:45</span
					>
				</div>
				<aside
					class="flex w-[78px] shrink-0 flex-col border-l border-hairline bg-ground"
					aria-label="Keep timeline"
				>
					<div
						class="flex h-6 shrink-0 items-center justify-center bg-live font-mono text-2xs tracking-[0.08em] text-white"
					>
						LIVE
					</div>
					<div class="flex flex-1 flex-col items-center gap-[5px] pt-1.5">
						<span class="h-[26px] w-3.5 shrink-0 bg-availability"></span>
						<span class="flex w-full items-center justify-center gap-[5px]">
							<span class="h-0.5 w-[34px] bg-primary"></span>
							<span
								class="flex h-4 items-center rounded-full bg-primary px-[5px] font-mono text-2xs text-on-primary"
							>
								45
							</span>
						</span>
						<span class="h-16 w-3.5 shrink-0 bg-availability"></span>
					</div>
				</aside>
			</div>
		</section>
	{/if}
</main>
