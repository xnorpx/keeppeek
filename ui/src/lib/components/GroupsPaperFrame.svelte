<script lang="ts">
	import { capabilityActions } from '$lib/capability-actions';
	import CapabilityGate from './CapabilityGate.svelte';
	import DesktopPaperRail from './DesktopPaperRail.svelte';

	type Props = {
		frame: 'administration' | 'participant';
	};

	let { frame }: Props = $props();

	const adminRows = [
		{
			title: 'Group directory unavailable',
			meta: 'COUNT UNAVAILABLE',
			members: 'No names or members',
			live: 'No participant state',
			recording: 'Unavailable',
			presence: 'Runtime absent'
		},
		{
			title: 'Client command boundary',
			meta: 'GENERATED TYPES',
			members: 'List · join · leave',
			live: 'Full duplex contract',
			recording: 'Not reported',
			presence: 'No handler'
		},
		{
			title: 'Definition ownership',
			meta: 'SERVER CONFIGURATION',
			members: 'Static streams only',
			live: 'No floor control',
			recording: 'Policy unavailable',
			presence: 'Not joinable'
		}
	] as const;

	const participantCards = [
		{ label: 'List', detail: 'UNAVAILABLE' },
		{ label: 'Join', detail: 'UNAVAILABLE' },
		{ label: 'Leave', detail: 'UNAVAILABLE' },
		{ label: 'Members', detail: 'NOT RETURNED' }
	] as const;
</script>

{#if frame === 'administration'}
	<section
		data-groups-paper-frame="administration"
		class="flex h-[416px] w-[1440px] overflow-hidden rounded-lg border border-hairline bg-surface [font-synthesis:none]"
		aria-label="Group administration evidence"
	>
		<DesktopPaperRail />

		<div class="flex h-[414px] w-[1374px] shrink-0 flex-col">
			<header
				class="flex h-[52px] w-[1374px] shrink-0 items-center justify-between border-b border-hairline px-5"
			>
				<div class="flex items-baseline gap-3">
					<h2 class="text-base leading-5 font-semibold">Settings</h2>
					<p class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-muted">
						GROUPS · ADMIN CONFIGURATION · TARGET group-admin.v1
					</p>
				</div>
				<CapabilityGate
					{...capabilityActions.newGroup}
					class="h-[30px] min-h-[30px] shrink-0 px-3.5 text-[11px]"
				/>
			</header>

			<div class="flex h-[326px] w-[1374px] shrink-0 flex-col gap-6 px-8 py-7">
				<div
					class="flex h-[30px] w-[1310px] shrink-0 items-center border-b border-hairline-strong font-mono text-2xs leading-[14px] tracking-[0.14em] text-text-faint"
				>
					<span class="w-[300px] shrink-0">GROUP</span>
					<span class="w-[300px] shrink-0">MEMBER STREAMS</span>
					<span class="w-[260px] shrink-0">LIVE CAPABILITY</span>
					<span class="w-[210px] shrink-0">RECORDING</span>
					<span class="w-[240px] shrink-0">IN THERE NOW</span>
				</div>

				{#each adminRows as row (row.title)}
					<div
						data-group-evidence-row
						class="flex h-14 w-[1310px] shrink-0 items-center border-b border-hairline"
					>
						<div class="flex w-[300px] shrink-0 flex-col gap-[3px] pr-4">
							<span class="text-sm leading-[18px] font-medium">{row.title}</span>
							<span class="font-mono text-2xs leading-[14px] text-text-faint">{row.meta}</span>
						</div>
						<span class="w-[300px] shrink-0 pr-4 text-[13px] leading-4 text-text-muted">
							{row.members}
						</span>
						<span
							class="flex w-[260px] shrink-0 items-center gap-2 pr-4 text-[13px] leading-4 text-text-muted"
						>
							<span class="size-1.5 rounded-full bg-text-faint"></span>{row.live}
						</span>
						<span class="w-[210px] shrink-0 text-[13px] leading-4 text-text-muted">
							{row.recording}
						</span>
						<span class="w-[240px] shrink-0 text-[13px] leading-4 text-text-faint">
							{row.presence}
						</span>
					</div>
				{/each}
			</div>
		</div>
	</section>
{:else}
	<section
		data-groups-paper-frame="participant"
		class="flex h-[420px] w-[1440px] items-start gap-5 overflow-hidden bg-ground [font-synthesis:none]"
		aria-label="Group participant contract"
	>
		<div class="flex h-[270px] w-[940px] shrink-0 flex-col gap-4">
			<div class="flex h-6 shrink-0 items-center gap-3">
				<h2 class="text-xl leading-6 font-semibold">What a participant sees</h2>
				<span class="h-px flex-1 bg-hairline"></span>
			</div>

			<div
				class="flex h-[230px] w-[940px] shrink-0 flex-col overflow-hidden rounded-md border border-hairline-strong bg-surface"
			>
				<header
					class="flex h-[72px] shrink-0 items-center justify-between border-b border-hairline px-[18px] py-4"
				>
					<div class="flex flex-col gap-[3px]">
						<h3 class="text-lg-plus leading-[22px] font-semibold">Participant state unavailable</h3>
						<p class="font-mono text-2xs leading-[14px] text-text-faint">
							LIST · JOIN · LEAVE HAVE GENERATED TYPES BUT NO RUNTIME HANDLER
						</p>
					</div>
					<span
						class="rounded-full border border-hairline-strong px-2.5 py-1 font-mono text-2xs text-text-faint"
					>
						NOT JOINED
					</span>
				</header>

				<div class="flex h-[156px] shrink-0 gap-3 p-[18px]">
					{#each participantCards as card (card.label)}
						<div
							data-participant-evidence-card
							class="flex h-[120px] w-[132px] shrink-0 flex-col items-center justify-center gap-2 rounded-md border border-dashed border-hairline-strong"
						>
							<span
								class="grid size-11 place-items-center rounded-full border border-dashed border-hairline-strong font-mono text-sm text-text-faint"
								>—</span
							>
							<span class="text-[13px] leading-4 text-text-muted">{card.label}</span>
							<span class="font-mono text-[10px] leading-3 tracking-[0.08em] text-text-faint">
								{card.detail}
							</span>
						</div>
					{/each}
					<div
						class="flex h-[120px] min-w-0 flex-1 flex-col justify-center gap-2.5 border-l border-hairline pl-4"
					>
						<button
							type="button"
							class="h-[52px] rounded-md bg-raised text-sm font-semibold text-text-faint"
							disabled
						>
							Join required for local talk control
						</button>
						<p class="text-center text-xs-plus leading-[18px] text-text-muted">
							Push-to-talk only stops local media production; it never requests a server floor.
						</p>
					</div>
				</div>
			</div>
		</div>

		<div class="flex h-[420px] w-[480px] shrink-0 flex-col gap-4">
			<div class="flex h-6 shrink-0 items-center gap-3">
				<h2 class="text-xl leading-6 font-semibold">What is deliberately absent</h2>
				<span class="h-px flex-1 bg-hairline"></span>
			</div>
			<div class="flex h-[380px] shrink-0 flex-col gap-3.5 border-l-2 border-primary bg-raised p-5">
				<div class="flex flex-col gap-1">
					<h3 class="text-sm leading-[18px] font-semibold">No floor control</h3>
					<p class="text-[13px] leading-[21px] text-text-muted">
						No request to speak, no grant, no queue, no half duplex. If two people talk over each
						other that is a human problem, and humans are good at it.
					</p>
				</div>
				<div class="flex flex-col gap-1">
					<h3 class="text-sm leading-[18px] font-semibold">
						Media clients only list, join and leave
					</h3>
					<p class="text-[13px] leading-[21px] text-text-muted">
						A client may only list, join and leave. Group definitions are server configuration,
						which is what makes their identifiers stable enough to record against.
					</p>
				</div>
				<div class="flex flex-col gap-1">
					<h3 class="text-sm leading-[18px] font-semibold">No dynamic members</h3>
					<p class="text-[13px] leading-[21px] text-text-muted">
						Only static camera streams can be members. Anything ephemeral — a client publication, a
						transcoded variant, another participant — would leave a dangling reference the moment it
						ended.
					</p>
				</div>
				<div class="flex flex-col gap-1">
					<h3 class="text-sm leading-[18px] font-semibold">Not in the capabilities snapshot</h3>
					<p class="text-[13px] leading-[21px] text-text-muted">
						That snapshot churns every time a camera connects. Groups are asked for separately and
						resolved to live sessions the same way any other source is.
					</p>
				</div>
			</div>
		</div>
	</section>
{/if}
