<script lang="ts">
	import { onMount } from 'svelte';
	import { create } from '@bufbuild/protobuf';
	import type { ControlClient } from '$lib/control-client';
	import type { RestoreRecord } from '$lib/proto/backup_pb';
	import {
		ConfigurationPlanSchema,
		type ConfigurationRestoreVerification
	} from '$lib/proto/webrtc_pb';
	import { validateRestorePreparation } from '$lib/configuration-restore-verification';
	import ExternalAuthenticationPlan from './ExternalAuthenticationPlan.svelte';
	import { Button } from './ui/button/index.js';

	type Controller = Pick<
		ControlClient,
		| 'onAccessState'
		| 'prepareConfigurationRestoreVerification'
		| 'getConfigurationRestoreVerification'
		| 'confirmConfigurationRestoreVerification'
		| 'prepareAdministratorVerification'
		| 'getAdministratorVerification'
		| 'verifyAdministratorBearer'
		| 'applyExternalAuthentication'
		| 'applyConfiguration'
	>;
	let {
		controller,
		file,
		onstaged,
		onbusy
	}: {
		controller: Controller;
		file: File;
		onstaged: (record: RestoreRecord) => void;
		onbusy?: (busy: boolean) => void;
	} = $props();
	let preparation = $state.raw<ConfigurationRestoreVerification | null>(null);
	let busy = $state(false);
	let received = $state(0);
	let error = $state<string | null>(null);
	let disconnected = $state(false);
	const lifetime = new AbortController();
	let active = false;
	let inspectedFile: File | null = null;
	let plan = $derived(
		preparation
			? create(ConfigurationPlanSchema, {
					planId: preparation.preparationId,
					expiresAtMs: preparation.expiresAtMs,
					valid: preparation.ready,
					requiresAdministratorConfirmation: preparation.requiresAdministratorConfirmation,
					applySemantics:
						'The exact inspected ZIP will replace config.toml and secrets.toml on restart. Current browser sessions may be signed out.'
				})
			: null
	);

	onMount(() => {
		active = true;
		let sessionId: string | undefined;
		let generation: number | undefined;
		const stop = controller.onAccessState((state) => {
			const remoteAdmin =
				state.status === 'authenticated' &&
				state.session?.role === 'administrator' &&
				!state.session.local;
			if (sessionId === undefined && remoteAdmin) {
				sessionId = state.session?.id;
				generation = state.generation;
			} else if (
				!remoteAdmin ||
				state.session?.id !== sessionId ||
				state.generation !== generation
			) {
				disconnected = true;
				lifetime.abort();
				preparation = null;
				error =
					'The control session changed. Select the archive again on a live remote Administrator session.';
			}
		});
		return () => {
			active = false;
			lifetime.abort();
			stop();
			onbusy?.(false);
		};
	});

	function guard(): void {
		lifetime.signal.throwIfAborted();
		if (!active || disconnected || (inspectedFile && file !== inspectedFile))
			throw new Error('The restore session or selected file changed.');
	}

	async function inspect(): Promise<void> {
		if (busy || disconnected) return;
		busy = true;
		error = null;
		preparation = null;
		received = 0;
		inspectedFile = file;
		try {
			guard();
			const result = await controller.prepareConfigurationRestoreVerification(
				file,
				lifetime.signal,
				(bytes) => {
					if (active && !disconnected) received = bytes;
				}
			);
			guard();
			validateRestorePreparation(result, result.preparationId, file.size, file.size);
			if (!result.ready) throw new Error('Restore inspection is not ready.');
			preparation = result;
		} catch (cause) {
			if (active && !disconnected)
				error = cause instanceof Error ? cause.message : 'Restore inspection failed.';
		} finally {
			if (active) busy = false;
		}
	}

	async function confirm(verificationId?: string): Promise<void> {
		guard();
		const prepared = preparation;
		const archive = inspectedFile;
		if (!prepared || !archive) throw new Error('Inspect the archive first.');
		onbusy?.(true);
		try {
			const latest = await controller.getConfigurationRestoreVerification(prepared.preparationId);
			guard();
			validateRestorePreparation(latest, prepared.preparationId, archive.size, archive.size);
			if (!latest.ready || (latest.requiresAdministratorConfirmation && !verificationId))
				throw new Error('Fresh replacement Administrator verification is required.');
			const admitted = await controller.confirmConfigurationRestoreVerification(
				prepared.preparationId,
				verificationId
			);
			guard();
			validateRestorePreparation(admitted, prepared.preparationId, archive.size, archive.size);
			if (!admitted.ready || !admitted.confirmed)
				throw new Error('Restore confirmation was not accepted.');
			const record = await controller.applyConfiguration(archive, lifetime.signal);
			guard();
			onstaged(record);
		} catch (cause) {
			if (active && !disconnected) {
				preparation = null;
				error =
					'Restore was not confirmed as staged. Select the archive again and inspect it before retrying.';
			}
			throw cause;
		} finally {
			if (active) onbusy?.(false);
		}
	}
</script>

<div class="space-y-3">
	<p class="text-xs text-text-muted">
		Inspect the archive on this remote control session before restoring. No configuration is changed
		during inspection.
	</p>
	{#if !preparation}
		<Button
			type="button"
			variant="outline"
			disabled={busy || disconnected}
			onclick={() => void inspect()}
		>
			{busy ? 'Inspecting archive…' : 'Inspect restore archive'}
		</Button>
	{/if}
	{#if busy}<p role="status" class="font-mono text-xs text-text-muted">
			Uploaded {received} of {file.size} bytes for inspection.
		</p>{/if}
	{#if preparation && plan}
		{#key preparation.preparationId}
			<ExternalAuthenticationPlan
				{controller}
				{plan}
				settings={preparation.candidateAuthentication ?? null}
				restore
				onconfirm={confirm}
				onapplied={() => {}}
			/>
		{/key}
	{/if}
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
</div>
