<script lang="ts">
	import { accessEvidence } from '$lib/access';
	import { capabilityActions } from '$lib/capability-actions';
	import CapabilityGate from '$lib/components/CapabilityGate.svelte';
	import CheckIcon from '@lucide/svelte/icons/check';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import KeyRoundIcon from '@lucide/svelte/icons/key-round';
	import MinusIcon from '@lucide/svelte/icons/minus';
	import ShieldIcon from '@lucide/svelte/icons/shield';
	import UserRoundIcon from '@lucide/svelte/icons/user-round';
	import UsersIcon from '@lucide/svelte/icons/users';
	import AccessPaperFrame from './AccessPaperFrame.svelte';

	type Props = {
		paperFrame?: boolean;
	};

	let { paperFrame = false }: Props = $props();

	const evidence = accessEvidence();
</script>

{#if paperFrame}
	<AccessPaperFrame />
{:else}
	<section
		id="access"
		class="scroll-mt-4 overflow-hidden rounded-md border border-hairline bg-surface"
		aria-labelledby="access-heading"
	>
		<header
			class="flex flex-wrap items-end justify-between gap-4 border-b border-hairline px-5 py-5"
		>
			<div class="max-w-2xl">
				<p class="font-mono text-2xs tracking-caps text-primary-soft">ACCESS & ROLES</p>
				<h2 id="access-heading" class="mt-1 text-xl font-semibold">
					Operation is not configuration
				</h2>
				<p class="mt-1 text-sm leading-6 text-text-muted">
					Paper defines two target roles: a User may view and operate; an Administrator may also
					configure. The current server exposes no identity runtime that enforces or reports this
					policy.
				</p>
			</div>
			<div class="flex flex-wrap gap-2">
				<CapabilityGate {...capabilityActions.inviteSomeone} />
				<CapabilityGate {...capabilityActions.newAccessToken} />
			</div>
		</header>

		<div class="border-b border-hairline bg-activity/5 px-5 py-4">
			<div class="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
				<div class="flex max-w-3xl gap-3">
					<CircleAlertIcon class="mt-0.5 size-4 shrink-0 text-activity" />
					<div>
						<p class="text-sm font-semibold">No runtime identity or authorization evidence</p>
						<p class="mt-1 text-xs leading-5 text-text-muted">
							The API design documents local Administrator bypass and one shared remote Bearer key,
							but the current Rust server has no access-key or identity handler. Neither model is
							presented as active enforcement here.
						</p>
					</div>
				</div>
				<CapabilityGate {...capabilityActions.enableRemoteSignIn} class="shrink-0" />
			</div>
		</div>

		<div class="border-b border-hairline p-5">
			<div class="mb-3 flex flex-wrap items-end justify-between gap-2">
				<div>
					<h3 class="text-base font-semibold">Target role matrix</h3>
					<p class="mt-1 text-xs text-text-muted">
						Authored policy only · not enforced by this server
					</p>
				</div>
				<span class="font-mono text-2xs tracking-caps text-activity">TARGET · IDENTITY.V1</span>
			</div>
			<div class="overflow-x-auto border-y border-hairline">
				<table class="w-full min-w-[44rem] text-left text-xs">
					<thead class="bg-raised text-text-faint">
						<tr>
							<th class="px-3 py-2 font-mono text-2xs tracking-caps">ACTION</th>
							<th class="w-36 px-3 py-2 text-center font-mono text-2xs tracking-caps">KIND</th>
							<th class="w-36 px-3 py-2 text-center font-mono text-2xs tracking-caps"
								>ADMINISTRATOR</th
							>
							<th class="w-36 px-3 py-2 text-center font-mono text-2xs tracking-caps">USER</th>
						</tr>
					</thead>
					<tbody class="divide-y divide-hairline">
						{#each evidence.permissions as permission (permission.label)}
							<tr>
								<td class="px-3 py-2.5">
									<span>{permission.label}</span>
									{#if permission.requiresCapability}<span
											class="ml-2 font-mono text-2xs text-text-faint"
											>REQUIRES {permission.requiresCapability}</span
										>{/if}
								</td>
								<td class="px-3 py-2.5 text-center font-mono text-2xs text-text-faint uppercase"
									>{permission.kind}</td
								>
								<td class="px-3 py-2.5"
									><CheckIcon
										class="mx-auto size-4 text-healthy"
										aria-label="Administrator target allows"
									/></td
								>
								<td class="px-3 py-2.5">
									{#if permission.user}
										<CheckIcon
											class="mx-auto size-4 text-healthy"
											aria-label="User target allows"
										/>
									{:else}
										<MinusIcon
											class="mx-auto size-4 text-text-faint"
											aria-label="User target excludes"
										/>
									{/if}
								</td>
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		</div>

		<div class="grid lg:grid-cols-2">
			<div class="space-y-4 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<div class="flex flex-wrap items-baseline justify-between gap-2">
					<h3 class="flex items-center gap-2 text-base font-semibold">
						<UsersIcon class="size-4" /> People & sessions
					</h3>
					<span class="font-mono text-2xs tracking-caps text-text-faint">COUNT UNAVAILABLE</span>
				</div>
				<div
					class="grid min-h-36 place-items-center rounded-sm border border-dashed border-hairline-strong bg-raised/40 p-4 text-center"
				>
					<div class="max-w-sm">
						<UserRoundIcon class="mx-auto size-6 text-text-faint" />
						<p class="mt-2 text-sm font-medium">Identity directory unavailable</p>
						<p class="mt-1 text-xs leading-5 text-text-muted">
							No people, assigned roles, invitations, sign-in sessions, last-seen times, or current
							identity are returned by any endpoint.
						</p>
					</div>
				</div>
				<CapabilityGate
					{...capabilityActions.managePeopleAndSessions}
					class="w-full justify-start"
				/>
			</div>

			<div class="space-y-4 p-5">
				<div class="flex flex-wrap items-baseline justify-between gap-2">
					<h3 class="flex items-center gap-2 text-base font-semibold">
						<KeyRoundIcon class="size-4" /> Access tokens
					</h3>
					<span class="font-mono text-2xs tracking-caps text-text-faint">COUNT UNAVAILABLE</span>
				</div>
				<div
					class="grid min-h-36 place-items-center rounded-sm border border-dashed border-hairline-strong bg-raised/40 p-4 text-center"
				>
					<div class="max-w-sm">
						<ShieldIcon class="mx-auto size-6 text-text-faint" />
						<p class="mt-2 text-sm font-medium">Token registry unavailable</p>
						<p class="mt-1 text-xs leading-5 text-text-muted">
							The documented shared key has no list, create, rotate, revoke, scope, owner, last-use,
							or audit endpoint. Raw key material is never rendered.
						</p>
					</div>
				</div>
				<CapabilityGate {...capabilityActions.manageAccessTokens} class="w-full justify-start" />
			</div>
		</div>

		<footer class="grid gap-3 border-t border-hairline bg-raised/40 px-5 py-4 sm:grid-cols-3">
			<div>
				<p class="font-mono text-2xs tracking-caps text-text-faint">DOCUMENTED LOCAL MODEL</p>
				<p class="mt-1 text-xs text-text-muted">Administrator without sign-in · not implemented</p>
			</div>
			<div>
				<p class="font-mono text-2xs tracking-caps text-text-faint">DOCUMENTED REMOTE MODEL</p>
				<p class="mt-1 text-xs text-text-muted">One shared Bearer key · not implemented</p>
			</div>
			<div>
				<p class="font-mono text-2xs tracking-caps text-text-faint">AUDIT TRAIL</p>
				<p class="mt-1 text-xs text-text-muted">
					Unavailable in both documented and current contracts
				</p>
			</div>
		</footer>
	</section>
{/if}
