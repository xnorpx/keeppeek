<script lang="ts">
	import { untrack } from 'svelte';
	import type { ControlClient } from '$lib/control-client';
	import {
		RECORDING_CONTROL_CAPABILITY,
		type RecordingControlClient
	} from '$lib/control-client-recording';
	import {
		CameraRecordingMode,
		RecordingControlReason,
		RecordingOverrideSource,
		type RecordingControlState
	} from '$lib/proto/webrtc_pb';
	import { Button } from './ui/button/index.js';
	import { Input } from './ui/input/index.js';
	type Props = {
		controller: Pick<ControlClient, 'onCapabilities' | 'onAccessState'> & {
			recordingControl: Pick<RecordingControlClient, 'get' | 'pause' | 'clear'>;
		};
		sourceId: string;
	};
	let { controller, sourceId }: Props = $props();
	const id = $props.id();
	let supported = $state(false);
	let administrator = $state(false);
	let accessIdentity = $state('');
	let snapshot = $state.raw<RecordingControlState | null>(null);
	let busy = $state(false);
	let error = $state<string | null>(null);
	let reason = $state('');
	let minutes = $state<number | undefined>(15);
	let generation = 0;
	const modes: Record<number, string> = {
		[CameraRecordingMode.SUB]: 'Sub',
		[CameraRecordingMode.MAIN]: 'Main',
		[CameraRecordingMode.BOTH]: 'Both',
		[CameraRecordingMode.EVENT_BOOST]: 'Event boost',
		[CameraRecordingMode.OFF]: 'Off'
	};
	const reasons: Record<number, string> = {
		[RecordingControlReason.CONFIGURATION]: 'Using configured recording mode.',
		[RecordingControlReason.CONFIGURED_DISABLED]: 'Recording is disabled in camera settings.',
		[RecordingControlReason.PRIVACY]: 'Recording is blocked by privacy.',
		[RecordingControlReason.PRIVACY_UNAVAILABLE]:
			'Recording is blocked because privacy state is unavailable.',
		[RecordingControlReason.OVERRIDE]: 'A temporary recording override is active.',
		[RecordingControlReason.EXPIRED]:
			'The temporary override expired. Using configured recording mode.',
		[RecordingControlReason.CLOCK_UNAVAILABLE]:
			'Recording is blocked because the clock is unavailable.'
	};

	$effect(() =>
		controller.onCapabilities((ids) => {
			if (!ids.includes(RECORDING_CONTROL_CAPABILITY)) generation += 1;
			supported = ids.includes(RECORDING_CONTROL_CAPABILITY);
		})
	);
	$effect(() =>
		controller.onAccessState((access) => {
			const identity = `${access.status}:${access.generation}:${access.session?.id}:${access.session?.principalId}:${access.session?.role}`;
			if (identity !== untrack(() => accessIdentity)) {
				generation += 1;
				snapshot = null;
			}
			accessIdentity = identity;
			administrator = access.status === 'authenticated' && access.session?.role === 'administrator';
		})
	);
	$effect(() => {
		const client = controller.recordingControl;
		const camera = sourceId;
		const available = supported && administrator;
		void accessIdentity;
		generation += 1;
		snapshot = null;
		error = null;
		busy = false;
		if (!available) return;
		// Refresh only when the source or access changes, not after each response.
		untrack(() => void run(() => client.get(camera)));
		const timer = setInterval(() => {
			if (!busy && !error) void run(() => client.get(camera));
		}, 15_000);
		return () => {
			generation += 1;
			clearInterval(timer);
		};
	});

	async function run(action: () => Promise<RecordingControlState>): Promise<void> {
		if (busy || !supported || !administrator) return;
		const current = generation;
		busy = true;
		error = null;
		try {
			const result = await action();
			if (current === generation) snapshot = result;
		} catch (failure) {
			if (current === generation) {
				snapshot = null;
				error = failure instanceof Error ? failure.message : 'Unable to update recording state.';
			}
		} finally {
			if (current === generation) busy = false;
		}
	}

	function pause(event: SubmitEvent): void {
		event.preventDefault();
		const current = snapshot;
		if (current)
			void run(() => controller.recordingControl.pause(current, reason, (minutes ?? 0) * 60_000));
	}
	function clear(): void {
		const current = snapshot;
		if (current) void run(() => controller.recordingControl.clear(current));
	}
</script>

<section
	class="space-y-4 rounded-md border border-hairline bg-surface p-4"
	aria-labelledby={`${id}-heading`}
>
	<h2 id={`${id}-heading`} class="text-sm font-semibold">Recording controls</h2>
	{#if !supported}
		<p class="text-sm text-muted-foreground">
			Temporary recording controls are unavailable on this server.
		</p>
	{:else if !administrator}
		<p class="text-sm text-muted-foreground">
			An administrator account is required to manage recording controls.
		</p>
	{:else}
		<div role="status" class="space-y-1 text-sm" aria-busy={busy}>
			{#if snapshot}
				<p>Configured: {modes[snapshot.configuredMode] ?? 'Unknown'}</p>
				<p>Effective: {modes[snapshot.effectiveMode] ?? 'Unknown'}</p>
				<p>{reasons[snapshot.reason] ?? 'Recording state is unknown.'}</p>
				{#if snapshot.overrideState}
					<p class="break-words">{snapshot.overrideState.reason}</p>
					<p class="break-words">
						Requested by {snapshot.overrideState.actor} ({snapshot.overrideState.source ===
						RecordingOverrideSource.MANUAL
							? 'manual'
							: 'external'})
					</p>
					<p>Expires: {new Date(Number(snapshot.overrideState.expiresAtMs)).toLocaleString()}</p>
				{/if}
			{:else if busy}
				<p>Loading recording state…</p>
			{:else}
				<p>Refresh to obtain current recording state.</p>
			{/if}
		</div>
		{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
		<div class="flex flex-wrap gap-2">
			<Button
				variant="outline"
				disabled={busy}
				onclick={() => void run(() => controller.recordingControl.get(sourceId))}
				>Refresh recording state</Button
			>
			{#if snapshot?.overrideState}<Button variant="outline" disabled={busy} onclick={clear}
					>End temporary override</Button
				>{/if}
		</div>
		<form class="space-y-3" onsubmit={pause}>
			<div class="space-y-1">
				<label for={`${id}-reason`} class="text-sm">Reason for pause</label>
				<Input id={`${id}-reason`} bind:value={reason} required maxlength={256} disabled={busy} />
			</div>
			<div class="space-y-1">
				<label for={`${id}-minutes`} class="text-sm">Pause duration (minutes)</label>
				<Input
					id={`${id}-minutes`}
					type="number"
					bind:value={minutes}
					required
					min={1}
					max={1440}
					step={1}
					disabled={busy}
				/>
			</div>
			<p class="text-xs text-muted-foreground">
				Pauses last up to 24 hours and end if the server restarts. Ending an override restores the
				configured mode, subject to privacy restrictions.
			</p>
			<Button
				type="submit"
				disabled={busy || !snapshot || snapshot.configuredMode === CameraRecordingMode.OFF}
				>Pause recording</Button
			>
		</form>
	{/if}
</section>
