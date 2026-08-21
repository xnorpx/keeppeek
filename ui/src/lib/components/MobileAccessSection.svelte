<script lang="ts">
	import KeyRoundIcon from '@lucide/svelte/icons/key-round';
	import ShieldAlertIcon from '@lucide/svelte/icons/shield-alert';
	import UsersIcon from '@lucide/svelte/icons/users';
</script>

<section
	data-mobile-access
	class="h-[660px] overflow-hidden p-4 md:hidden"
	aria-label="Mobile access"
>
	<div class="flex h-[81px] gap-2.5 rounded-sm border border-healthy/50 bg-healthy/5 p-3">
		<ShieldAlertIcon class="mt-0.5 size-3.5 shrink-0 text-healthy" />
		<div class="min-w-0">
			<h2 class="text-sm leading-4 font-semibold">Identity runtime unavailable</h2>
			<p class="mt-1 text-xs leading-[18px] text-text-muted">
				Loopback and shared-key HTTP access are active; identities and roles remain unavailable.
			</p>
		</div>
	</div>

	<div class="mt-[14px] h-[109px]">
		<h3 class="h-5 text-lg leading-5 font-semibold">Two fixed roles</h3>
		<div class="mt-2 grid h-[81px] grid-cols-2 gap-2">
			<div class="rounded-sm border border-primary bg-raised p-3">
				<p class="text-md leading-[18px] font-semibold">Administrator</p>
				<p class="mt-1 text-xs leading-4 text-text-muted">Target policy includes configuration.</p>
			</div>
			<div class="rounded-sm border border-hairline bg-raised p-3">
				<p class="text-md leading-[18px] font-semibold">User</p>
				<p class="mt-1 text-xs leading-4 text-text-muted">Target policy covers operation only.</p>
			</div>
		</div>
	</div>

	<div class="mt-[14px] flex h-5 items-center justify-between">
		<h3 class="text-lg leading-5 font-semibold">People</h3>
		<span class="text-sm leading-4 text-text-faint">—</span>
	</div>
	<div
		class="mt-2.5 grid h-[158px] place-items-center rounded-md border border-hairline bg-surface p-4 text-center"
	>
		<div>
			<UsersIcon class="mx-auto size-5 text-text-faint" />
			<p class="mt-2 text-sm leading-4 font-medium">Identity directory unavailable</p>
			<p class="mt-1 text-xs leading-4 text-text-muted">
				No people, roles, or sessions are returned.
			</p>
		</div>
	</div>

	<div class="mt-[14px] flex h-5 items-center justify-between">
		<h3 class="text-lg leading-5 font-semibold">Tokens</h3>
		<span class="text-sm leading-4 text-text-faint">—</span>
	</div>
	<div
		class="mt-2.5 grid h-[94px] place-items-center rounded-md border border-hairline bg-surface px-4 text-center"
	>
		<div class="flex items-center gap-2">
			<KeyRoundIcon class="size-4 text-text-faint" />
			<div class="text-left">
				<p class="text-sm leading-4 font-medium">Token registry unavailable</p>
				<p class="mt-1 text-xs leading-4 text-text-muted">No key names or scopes are returned.</p>
			</div>
		</div>
	</div>
</section>
