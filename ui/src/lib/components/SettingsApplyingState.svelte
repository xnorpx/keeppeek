<script lang="ts">
	type Props = {
		fieldLabel: string;
		confirmedValue: string;
		actionLabel?: string;
		lockLabel?: string;
		detail: string;
		class?: string;
	};

	let {
		fieldLabel,
		confirmedValue,
		actionLabel = 'Applying…',
		lockLabel = 'Fields locked until it lands',
		detail,
		class: className = ''
	}: Props = $props();
</script>

<div
	data-settings-applying
	class="flex flex-col gap-3.5 bg-raised p-[18px] {className}"
	role="status"
>
	<div class="flex flex-col gap-1.5">
		<span class="font-mono text-2xs leading-3 tracking-caps text-text-faint uppercase">
			{fieldLabel}
		</span>
		<span
			class="flex h-[34px] items-center rounded-sm border border-hairline bg-surface px-3 text-sm leading-4 text-text-faint"
			aria-disabled="true"
		>
			{confirmedValue}
		</span>
	</div>
	<div class="flex items-center gap-2.5">
		<span
			class="inline-flex h-[34px] shrink-0 items-center gap-2 rounded-sm bg-primary-deep px-3.5 text-sm leading-4 font-semibold text-on-primary"
		>
			<span class="size-1.5 rounded-full bg-on-primary"></span>
			{actionLabel}
		</span>
		<span class="font-mono text-2xs leading-3 tracking-caps text-text-faint uppercase">
			{lockLabel}
		</span>
	</div>
	<div class="rounded-sm border border-[#E8A33D57] bg-[#E8A33D17] px-3.5 py-3">
		<p class="text-sm leading-[19px] text-text-muted">{detail}</p>
	</div>
</div>
