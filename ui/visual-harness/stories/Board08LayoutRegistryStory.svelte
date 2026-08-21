<script lang="ts">
	import Grid2X2Icon from '@lucide/svelte/icons/grid-2x2';
	import InfoIcon from '@lucide/svelte/icons/info';
</script>

<main
	data-paper-scenario="peek.desktop.layout-registry"
	class="flex h-[396px] w-[1440px] shrink-0 gap-7 overflow-hidden border-t border-hairline bg-ground pt-11 [font-synthesis:none]"
>
	<section
		class="flex h-[351px] w-[706px] shrink-0 flex-col gap-4"
		aria-label="Layout switcher evidence"
	>
		<h1 class="h-6 shrink-0 text-xl leading-6 font-semibold">Layout switcher</h1>
		<div
			class="grid h-[171px] shrink-0 place-items-center rounded-lg border border-hairline-strong bg-surface px-8 text-center"
		>
			<div class="space-y-2">
				<Grid2X2Icon class="mx-auto size-4 text-text-faint" />
				<p class="text-sm font-medium">Server layout registry unavailable</p>
				<p class="text-xs text-text-muted">
					No saved layout names, camera counts, or create command were returned.
				</p>
			</div>
		</div>
		<p class="text-[13px] leading-5 text-text-muted">
			The current browser draft is local and is never presented as a server-owned layout.
		</p>
	</section>

	<section
		class="flex h-[351px] w-[706px] shrink-0 flex-col gap-4"
		aria-label="Layout deletion evidence"
	>
		<h1 class="h-6 shrink-0 text-xl leading-6 font-semibold">Deleting is deliberate</h1>
		<div
			class="flex h-[255px] shrink-0 flex-col gap-4 rounded-lg border border-hairline-strong bg-surface p-6"
		>
			<div class="flex items-center gap-2">
				<InfoIcon class="size-4 text-text-faint" />
				<h2 class="text-lg-plus leading-[22px] font-semibold">No server layout selected</h2>
			</div>
			<p class="text-[13px] leading-5 text-text-muted">
				A destructive confirmation cannot be offered until the server returns a stable layout
				identity and deletion contract.
			</p>
			<label class="flex flex-col gap-[7px] text-xs text-text-muted">
				Layout name
				<input
					type="text"
					class="h-[38px] rounded-sm border border-hairline-strong bg-raised px-3 font-mono text-[13px] text-text-faint"
					placeholder="Not returned"
					disabled
				/>
			</label>
			<div class="flex justify-end gap-2.5">
				<button
					type="button"
					class="h-8 rounded-sm border border-hairline bg-raised px-3.5 text-[13px]">Cancel</button
				>
				<button
					type="button"
					class="h-8 rounded-sm bg-hairline px-3.5 text-[13px] font-semibold text-live-text"
					disabled>Delete layout</button
				>
			</div>
		</div>
		<p class="text-[13px] leading-5 text-text-muted">
			Deletion remains unavailable rather than targeting a fixture-only layout.
		</p>
	</section>
</main>
