<script lang="ts">
	import { capabilityActions } from '$lib/capability-actions';
	import CapabilityGate from '$lib/components/CapabilityGate.svelte';
	import { groupEvidence } from '$lib/groups';
	import CameraIcon from '@lucide/svelte/icons/camera';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import ListIcon from '@lucide/svelte/icons/list';
	import LockIcon from '@lucide/svelte/icons/lock';
	import MicIcon from '@lucide/svelte/icons/mic';
	import RadioIcon from '@lucide/svelte/icons/radio';
	import ServerIcon from '@lucide/svelte/icons/server';
	import UsersIcon from '@lucide/svelte/icons/users';
	import GroupsPaperFrame from './GroupsPaperFrame.svelte';

	type Props = {
		paperFrame?: 'administration' | 'participant';
	};

	let { paperFrame }: Props = $props();

	const evidence = groupEvidence();
</script>

{#if paperFrame}
	<GroupsPaperFrame frame={paperFrame} />
{:else}
	<section
		id="groups"
		class="scroll-mt-4 overflow-hidden rounded-md border border-hairline bg-surface"
		aria-labelledby="groups-heading"
	>
		<header
			class="flex flex-wrap items-end justify-between gap-4 border-b border-hairline px-5 py-5"
		>
			<div class="max-w-2xl">
				<p class="font-mono text-2xs tracking-caps text-primary-soft">
					GROUPS · SERVER CONFIGURATION
				</p>
				<h2 id="groups-heading" class="mt-1 text-xl font-semibold">Groups & two-way audio</h2>
				<p class="mt-1 text-sm leading-6 text-text-muted">
					Groups are stable, server-defined collections of static camera streams. A group may also
					host full-duplex participants, but clients never create or mutate definitions through the
					media API.
				</p>
			</div>
			<CapabilityGate {...capabilityActions.newGroup} class="shrink-0" />
		</header>

		<div class="grid border-b border-hairline lg:grid-cols-[1.1fr_0.9fr]">
			<div class="space-y-4 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<div class="flex flex-wrap items-baseline justify-between gap-2">
					<h3 class="text-base font-semibold">Configured group directory</h3>
					<span class="font-mono text-2xs tracking-caps text-text-faint">COUNT UNAVAILABLE</span>
				</div>
				<div
					class="grid min-h-40 place-items-center rounded-sm border border-dashed border-hairline-strong bg-raised/40 p-5 text-center"
					role="status"
				>
					<div class="max-w-md space-y-3">
						<UsersIcon class="mx-auto size-7 text-text-faint" />
						<div>
							<h4 class="text-sm font-semibold">Group directory unavailable</h4>
							<p class="mt-1 text-xs leading-5 text-text-muted">
								The protobuf contract defines ListGroups, JoinGroup, and LeaveGroup, but this server
								and browser runtime do not handle those commands. No group names, members,
								participant counts, or recording states can be shown.
							</p>
						</div>
					</div>
				</div>
				<CapabilityGate
					{...capabilityActions.manageGroupDefinitions}
					class="w-full justify-start"
				/>
			</div>

			<div class="space-y-4 p-5">
				<h3 class="text-base font-semibold">Client command boundary</h3>
				<div class="grid grid-cols-3 gap-2 text-center">
					{#each evidence.clientCommands as command (command)}
						<div class="rounded-sm border border-hairline bg-raised px-2 py-3">
							<ListIcon class="mx-auto size-4 text-text-faint" />
							<p class="mt-1 font-mono text-xs capitalize">{command}</p>
						</div>
					{/each}
				</div>
				<div class="rounded-sm border border-activity/45 bg-activity/5 px-3 py-3">
					<div class="flex items-center gap-2 font-mono text-2xs tracking-caps text-activity">
						<CircleAlertIcon class="size-3.5" /> GENERATED TYPES · NO RUNTIME HANDLER
					</div>
					<p class="mt-1.5 text-xs leading-5 text-text-muted">
						Protocol declarations are not treated as live directory evidence. Until the
						control-channel handler ships, even list/join/leave remain unavailable in this UI.
					</p>
				</div>
			</div>
		</div>

		<div class="grid lg:grid-cols-2">
			<div class="space-y-3 border-b border-hairline p-5 lg:border-r lg:border-b-0">
				<h3 class="text-base font-semibold">Definition contract</h3>
				<dl class="divide-y divide-hairline border-y border-hairline text-xs">
					<div class="flex items-center justify-between gap-3 py-2.5">
						<dt class="flex items-center gap-2 text-text-muted">
							<ServerIcon class="size-3.5" /> Owner
						</dt>
						<dd class="font-mono">Server configuration</dd>
					</div>
					<div class="flex items-center justify-between gap-3 py-2.5">
						<dt class="flex items-center gap-2 text-text-muted">
							<CameraIcon class="size-3.5" /> Definition members
						</dt>
						<dd class="font-mono">Static camera streams only</dd>
					</div>
					<div class="flex items-center justify-between gap-3 py-2.5">
						<dt class="flex items-center gap-2 text-text-muted">
							<RadioIcon class="size-3.5" /> Live capability
						</dt>
						<dd class="font-mono">Optional audio / video</dd>
					</div>
					<div class="flex items-center justify-between gap-3 py-2.5">
						<dt class="flex items-center gap-2 text-text-muted">
							<LockIcon class="size-3.5" /> Join password
						</dt>
						<dd class="font-mono">Optional · never returned</dd>
					</div>
					<div class="flex items-center justify-between gap-3 py-2.5">
						<dt class="text-text-muted">Capability snapshot membership</dt>
						<dd class="font-mono">Deliberately absent</dd>
					</div>
				</dl>
			</div>

			<div class="space-y-4 p-5">
				<h3 class="flex items-center gap-2 text-base font-semibold">
					<MicIcon class="size-4" /> Live media behavior
				</h3>
				<div class="rounded-sm border border-primary/40 bg-primary/5 px-3 py-3">
					<p class="text-sm font-medium">Always full duplex</p>
					<p class="mt-1 text-xs leading-5 text-text-muted">
						Every joined participant may publish simultaneously. KeepPeek does not suppress
						overlapping speech or grant a turn to one participant.
					</p>
				</div>
				<div class="space-y-2 text-xs leading-5 text-text-muted">
					<p>
						<strong class="text-foreground">No floor control.</strong> No request-to-speak, grant, queue,
						or half-duplex mode exists.
					</p>
					<p>
						<strong class="text-foreground">Push-to-talk is local.</strong> A client stops producing audio
						while the publication remains active and warm.
					</p>
					<p>
						<strong class="text-foreground">Participant state is authoritative.</strong> It would come
						from GroupState after a successful join; it cannot be inferred from cameras or WebRTC session
						totals.
					</p>
				</div>
			</div>
		</div>
	</section>
{/if}
