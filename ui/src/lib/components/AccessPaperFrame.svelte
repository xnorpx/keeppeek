<script lang="ts">
	import { accessEvidence } from '$lib/access';
	import { capabilityActions } from '$lib/capability-actions';
	import CheckIcon from '@lucide/svelte/icons/check';
	import MinusIcon from '@lucide/svelte/icons/minus';
	import CapabilityGate from './CapabilityGate.svelte';
	import DesktopPaperRail from './DesktopPaperRail.svelte';

	const evidence = accessEvidence();
	const paperPermissionLabels: Record<string, string> = {
		'Open stored recordings': 'Scrub the timeline and open any recording',
		'Operate camera PTZ and presets': 'Pan, tilt, zoom and recall a preset',
		'Join a group and publish local media': 'Join a group and talk',
		'Export a clip or still': 'Export a clip or a still',
		'Configure cameras': 'Add, remove or reconfigure a camera',
		'Configure storage and services': 'Change retention, event sources or integrations',
		'Manage identities and tokens': 'Invite people, set roles, issue and revoke tokens'
	};
</script>

<section
	data-access-paper-frame
	class="flex h-[1249px] w-[1440px] overflow-hidden rounded-lg border border-hairline bg-surface [font-synthesis:none]"
	aria-label="Access and roles evidence"
>
	<DesktopPaperRail />

	<div class="flex h-[1247px] w-[1374px] shrink-0 flex-col">
		<header
			class="flex h-[52px] w-[1374px] shrink-0 items-center justify-between border-b border-hairline px-5"
		>
			<div class="flex items-baseline gap-3">
				<h2 class="text-base leading-5 font-semibold">Settings</h2>
				<p class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-muted">ACCESS</p>
			</div>
			<div class="flex items-center gap-2.5">
				<CapabilityGate
					{...capabilityActions.inviteSomeone}
					class="h-[30px] min-h-[30px] px-3 text-[11px]"
				/>
				<CapabilityGate
					{...capabilityActions.newAccessToken}
					class="h-[30px] min-h-[30px] px-3.5 text-[11px]"
				/>
			</div>
		</header>

		<div class="flex h-[1195px] w-[1374px] shrink-0 flex-col gap-7 px-8 py-7">
			<section
				data-access-band="reality"
				class="flex h-[102px] w-[1310px] shrink-0 items-start gap-3.5 rounded-md border border-activity/40 bg-activity/10 px-5 py-[18px]"
				aria-label="Runtime identity boundary"
			>
				<span class="mt-1 size-2 shrink-0 rounded-full bg-activity"></span>
				<div class="flex w-[900px] shrink-0 flex-col gap-1">
					<h3 class="text-[15px] leading-[18px] font-semibold">
						No runtime identity or authorization evidence
					</h3>
					<p class="text-[13px] leading-[21px] text-text-muted">
						The target design distinguishes operation from configuration. People, roles, sessions,
						tokens, and enforcement remain unavailable until the server advertises identity.v1.
					</p>
				</div>
				<div class="flex flex-1 justify-end">
					<CapabilityGate
						{...capabilityActions.enableRemoteSignIn}
						class="h-8 min-h-8 w-[323px] shrink-0 px-3.5 text-[11px]"
					/>
				</div>
			</section>

			<section
				data-access-band="matrix"
				class="flex h-[416px] w-[1310px] shrink-0 flex-col"
				aria-labelledby="access-matrix-heading"
			>
				<header class="flex h-16 shrink-0 items-end border-b border-hairline-strong pb-2.5">
					<h3
						id="access-matrix-heading"
						class="w-[600px] shrink-0 text-lg leading-[22px] font-semibold"
					>
						Who may do what
					</h3>
					<div class="flex w-[355px] shrink-0 flex-col gap-0.5">
						<span class="text-[15px] leading-[18px] font-semibold">Administrator</span>
						<span class="font-mono text-2xs leading-[14px] text-text-faint">TARGET ROLE</span>
					</div>
					<div class="flex w-[355px] shrink-0 flex-col gap-0.5">
						<span class="text-[15px] leading-[18px] font-semibold">User</span>
						<span class="font-mono text-2xs leading-[14px] text-text-faint">VIEW AND OPERATE</span>
					</div>
				</header>

				{#each evidence.permissions as permission (permission.label)}
					<div
						data-access-permission
						class="flex h-11 w-[1310px] shrink-0 items-center border-b border-hairline"
					>
						<div class="flex w-[600px] shrink-0 items-baseline gap-2.5">
							<span class="text-sm leading-[18px]">
								{paperPermissionLabels[permission.label] ?? permission.label}
							</span>
							<span class="font-mono text-[10px] leading-3 tracking-[0.08em] text-text-faint">
								{permission.requiresCapability
									? `REQUIRES ${permission.requiresCapability}`
									: permission.kind.toUpperCase()}
							</span>
						</div>
						<span class="w-[355px] shrink-0">
							<CheckIcon class="size-4 text-healthy" aria-label="Administrator target allows" />
						</span>
						<span class="w-[355px] shrink-0">
							{#if permission.user}
								<CheckIcon class="size-4 text-healthy" aria-label="User target allows" />
							{:else}
								<MinusIcon class="size-4 text-text-faint" aria-label="User target excludes" />
							{/if}
						</span>
					</div>
				{/each}
			</section>

			<section
				data-access-band="directory"
				class="flex h-[395px] w-[1310px] shrink-0 items-start gap-5"
				aria-label="Identity and token registries"
			>
				<article
					class="flex h-[274px] w-[645px] shrink-0 flex-col rounded-md border border-hairline bg-surface p-[18px]"
				>
					<header class="flex h-[34px] shrink-0 items-baseline justify-between pb-3">
						<h3 class="text-lg leading-[22px] font-semibold">People</h3>
						<span class="font-mono text-2xs leading-[14px] text-text-faint">COUNT UNAVAILABLE</span>
					</header>
					<div
						class="flex h-[26px] shrink-0 items-center border-b border-hairline-strong font-mono text-2xs leading-[14px] tracking-[0.14em] text-text-faint"
					>
						<span class="w-[200px]">NAME</span><span class="w-[150px]">ROLE</span><span
							class="w-[150px]">LAST SEEN</span
						>
					</div>
					{#each ['Identity directory unavailable', 'Assigned roles unavailable', 'Session history unavailable', 'Current identity unavailable'] as row (row)}
						<div
							class="flex h-11 shrink-0 items-center border-b border-hairline text-sm text-text-muted"
						>
							<span class="w-[200px]">{row}</span><span
								class="w-[150px] font-mono text-xs text-text-faint">—</span
							><span class="font-mono text-xs text-text-faint">—</span>
						</div>
					{/each}
				</article>

				<article
					class="flex h-[395px] w-[645px] shrink-0 flex-col rounded-md border border-hairline bg-surface p-[18px]"
				>
					<header class="flex h-[34px] shrink-0 items-baseline justify-between pb-3">
						<h3 class="text-lg leading-[22px] font-semibold">Tokens</h3>
						<span class="font-mono text-2xs leading-[14px] text-text-faint">COUNT UNAVAILABLE</span>
					</header>
					<div
						class="flex h-[26px] shrink-0 items-center border-b border-hairline-strong font-mono text-2xs leading-[14px] tracking-[0.14em] text-text-faint"
					>
						<span class="w-[230px]">USED BY</span><span class="w-[190px]">MAY DO</span><span
							class="w-[110px]">LAST USED</span
						>
					</div>
					{#each ['Token registry unavailable', 'Scope evidence unavailable', 'Rotation history unavailable', 'Revocation state unavailable'] as row (row)}
						<div
							class="flex h-11 shrink-0 items-center border-b border-hairline text-sm text-text-muted"
						>
							<span class="w-[230px]">{row}</span><span
								class="w-[190px] font-mono text-xs text-text-faint">—</span
							><span class="font-mono text-xs text-text-faint">—</span>
						</div>
					{/each}
					<div
						class="flex h-[121px] shrink-0 flex-col gap-1.5 border-t border-hairline bg-activity/5 px-[18px] py-[13px]"
					>
						<p class="font-mono text-2xs leading-3 tracking-caps text-activity">
							AUDIT TRAIL UNAVAILABLE
						</p>
						<p class="text-sm leading-[19px] text-text-muted">
							No immutable export, identity, token-use, scope, owner, or last-action rows exist. Raw
							key material is never rendered.
						</p>
					</div>
				</article>
			</section>

			<section
				data-access-band="decision"
				class="flex h-[142px] w-[1310px] shrink-0 items-start gap-10 border-t border-hairline pt-2"
				aria-label="Local and remote access model"
			>
				<div class="flex w-[400px] shrink-0 flex-col gap-2 pt-[18px]">
					<p class="font-mono text-2xs leading-[14px] tracking-[0.14em] text-activity">
						DOCUMENTED MODEL · NOT RUNTIME EVIDENCE
					</p>
					<h3 class="text-xl leading-6 font-semibold">Local open. Remote key designed.</h3>
				</div>
				<div
					class="flex w-[430px] shrink-0 flex-col gap-2.5 pt-[18px] text-[13px] leading-[21px] text-text-muted"
				>
					<p>
						Local Administrator bypass and remote Bearer access are documented design boundaries.
					</p>
					<p>No identity handler currently proves either role or a signed-in person in this UI.</p>
				</div>
				<div
					class="flex w-[400px] shrink-0 flex-col gap-2.5 border-l-2 border-primary bg-raised p-[18px]"
				>
					<h3 class="text-sm leading-[18px] font-semibold">Operation is not configuration</h3>
					<p class="text-[13px] leading-[21px] text-text-muted">
						Users may operate cameras in the target policy. Only administrators configure. No
						per-camera role builder is defined.
					</p>
				</div>
			</section>
		</div>
	</div>
</section>
