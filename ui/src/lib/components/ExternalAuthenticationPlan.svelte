<script lang="ts">
	import { onMount } from 'svelte';
	import type { ControlClient } from '$lib/control-client';
	import type {
		AdministratorVerification,
		ConfigurationPlan,
		ExternalAuthenticationSettings
	} from '$lib/proto/webrtc_pb';
	import { openAdministratorVerification } from '$lib/external-authentication.svelte';
	import { Button } from './ui/button/index.js';
	import { Input } from './ui/input/index.js';
	type Controller = Pick<
		ControlClient,
		| 'onAccessState'
		| 'prepareAdministratorVerification'
		| 'getAdministratorVerification'
		| 'verifyAdministratorBearer'
		| 'applyExternalAuthentication'
	>;
	let {
		controller,
		plan,
		settings,
		onapplied,
		onconfirm,
		restore = false
	}: {
		controller: Controller;
		plan: ConfigurationPlan;
		settings: ExternalAuthenticationSettings | null;
		onapplied: () => void;
		onconfirm?: (verificationId?: string) => Promise<void>;
		restore?: boolean;
	} = $props();
	let proof = $state.raw<AdministratorVerification | null>(null);
	let providerId = $state('');
	let selectedOrigin = $state('');
	let origins = $derived.by(() => {
		const provider = settings?.providers.find((item) => item.providerId === providerId);
		if (!provider || !settings) return [];
		if (provider.method.case !== 'oidc') return settings.allowedOrigins;
		try {
			const origin = new URL(provider.method.value.redirectUri).origin;
			return settings.allowedOrigins.filter((value) => value === origin);
		} catch {
			return [];
		}
	});
	let origin = $derived(origins.includes(selectedOrigin) ? selectedOrigin : (origins[0] ?? ''));
	let bearer = $state('');
	let confirmed = $state(false);
	let busy = $state(false);
	let error = $state<string | null>(null);
	let now = $state(Date.now());
	let disconnected = $state(false);
	let active = true;
	let timer: ReturnType<typeof setTimeout> | undefined;
	let closeHandoff: (() => void) | undefined;
	let polls = 0;
	let expired = $derived(now >= Number(plan.expiresAtMs));
	let verified = $derived(
		Boolean(
			proof?.verified &&
			proof.configurationPlanId === plan.planId &&
			Number(proof.expiresAtMs) > now
		)
	);
	let canApply = $derived(
		plan.valid &&
			!expired &&
			!disconnected &&
			confirmed &&
			(!plan.requiresAdministratorConfirmation || verified)
	);

	onMount(() => {
		let sessionId: string | undefined;
		const stop = controller.onAccessState((state) => {
			if (sessionId === undefined && state.status === 'authenticated')
				sessionId = state.session?.id;
			else if (state.status !== 'authenticated' || state.session?.id !== sessionId) {
				disconnected = true;
				proof = null;
				confirmed = false;
				clearTimeout(timer);
				closeHandoff?.();
				error = 'The control session changed. Reload settings and create a new plan.';
			}
		});
		// The mounted preview updates only its expiry clock; verification polling is separately bounded.
		const clock = setInterval(() => {
			now = Date.now();
		}, 1000);
		return () => {
			active = false;
			stop();
			clearInterval(clock);
			clearTimeout(timer);
			closeHandoff?.();
			bearer = '';
		};
	});

	async function prepare(): Promise<void> {
		clearTimeout(timer);
		closeHandoff?.();
		busy = true;
		error = null;
		proof = null;
		confirmed = false;
		try {
			const next = await controller.prepareAdministratorVerification(
				plan.planId,
				providerId,
				origin
			);
			if (active && !disconnected) proof = next;
		} catch {
			if (active)
				error =
					'Verification could not be prepared. Check the provider, origin, and remote administrator session.';
		} finally {
			if (active) busy = false;
		}
	}

	function openProof(): void {
		if (!proof?.browserStart || expired || disconnected) return;
		try {
			clearTimeout(timer);
			closeHandoff?.();
			closeHandoff = openAdministratorVerification({
				origin: proof.browserStart.origin,
				provider_id: proof.browserStart.providerId,
				csrf_token: proof.browserStart.csrfToken,
				candidate_plan_id: proof.verificationId
			});
			polls = 0;
			void poll(proof.verificationId);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Allow the verification window, then retry.';
		}
	}

	async function poll(id: string): Promise<void> {
		if (!active || disconnected || proof?.verificationId !== id) return;
		if (
			++polls > 150 ||
			Date.now() >= Math.min(Number(plan.expiresAtMs), Number(proof?.expiresAtMs ?? 0))
		) {
			proof = null;
			confirmed = false;
			error = 'Verification expired. Prepare a new verification or preview a new plan.';
			return;
		}
		try {
			const next = await controller.getAdministratorVerification(id);
			if (!active || disconnected || proof?.verificationId !== id) return;
			if (next.configurationPlanId !== plan.planId || next.verificationId !== id)
				throw new Error('Verification binding changed.');
			proof = next;
			if (!next.verified) timer = setTimeout(() => void poll(id), 2000);
		} catch {
			if (active && proof?.verificationId === id) {
				proof = null;
				confirmed = false;
				error = 'Verification failed, expired, or was revoked. Prepare it again.';
			}
		}
	}

	async function verifyBearer(): Promise<void> {
		clearTimeout(timer);
		closeHandoff?.();
		proof = null;
		busy = true;
		error = null;
		confirmed = false;
		try {
			const next = await controller.verifyAdministratorBearer(plan.planId, bearer);
			if (active && !disconnected) proof = next;
		} catch {
			if (active) {
				proof = null;
				error = 'Replacement administrator key verification failed.';
			}
		} finally {
			bearer = '';
			if (active) busy = false;
		}
	}

	async function apply(): Promise<void> {
		now = Date.now();
		if (!canApply || busy) return;
		busy = true;
		error = null;
		try {
			const verificationId = plan.requiresAdministratorConfirmation
				? proof?.verificationId
				: undefined;
			if (onconfirm) await onconfirm(verificationId);
			else {
				const result = await controller.applyExternalAuthentication(plan, verificationId);
				if (!result.configurationCommitted) throw new Error('Not committed');
			}
			if (active) onapplied();
		} catch {
			if (active) {
				error = restore
					? 'Restore was not confirmed as staged. Inspect the selected archive again before retrying.'
					: 'Settings were not confirmed as applied. Reload and preview a fresh plan before retrying.';
				proof = null;
				confirmed = false;
			}
		} finally {
			if (active) busy = false;
		}
	}
</script>

<section
	class="space-y-4 border border-hairline bg-raised/30 p-4"
	aria-label="Authentication plan preview"
>
	<h4 class="text-sm font-semibold">
		{restore ? 'Review configuration restore' : 'Review authentication changes'}
	</h4>
	<p class="text-xs text-text-muted">{plan.applySemantics}</p>
	{#each plan.changes as change, index (index)}<p class="text-sm">
			{change.field}: {change.oldConfiguredValue} → {change.newConfiguredValue}
		</p>{/each}
	{#each plan.issues as issue, index (index)}<p class="text-sm" role="alert">
			{issue.field}: {issue.message}
		</p>{/each}
	{#if expired}<p role="alert" class="text-sm text-destructive">
			This plan expired. Preview the changes again.
		</p>{/if}
	{#if plan.requiresAdministratorConfirmation && !verified}
		<p class="text-sm">Verify a replacement Administrator before applying this change.</p>
		{#if settings}
			<label class="block text-xs"
				>Verification provider<select
					aria-label="Verification provider"
					class="mt-1 h-9 w-full rounded-sm border border-input bg-background px-3"
					bind:value={providerId}
					><option value="">Select a provider</option
					>{#each settings.providers as provider (provider.providerId)}<option
							value={provider.providerId}>{provider.name}</option
						>{/each}</select
				></label
			>
			{#if origins.length > 1}
				<label class="block text-xs"
					>Verification origin<select
						aria-label="Verification origin"
						class="mt-1 h-9 w-full rounded-sm border border-input bg-background px-3"
						value={origin}
						onchange={(event) => (selectedOrigin = event.currentTarget.value)}
						>{#each origins as value (value)}<option {value}>{value}</option>{/each}</select
					></label
				>
			{:else if origin}
				<p class="text-xs break-all text-text-muted">Verification origin: {origin}</p>
			{/if}
			<Button
				type="button"
				variant="outline"
				disabled={busy || expired || disconnected || !providerId || !origin}
				onclick={() => void prepare()}>Prepare verification</Button
			>
			{#if proof?.browserStart}<Button
					type="button"
					disabled={expired || disconnected}
					onclick={openProof}>Open verification window</Button
				>{/if}
		{/if}
		{#if !settings || settings.bearerEnabled}
			<label class="block space-y-1 text-xs"
				>Replacement Administrator access key<Input
					type="password"
					bind:value={bearer}
					autocomplete="off"
					maxlength={128}
				/></label
			>
			<Button
				type="button"
				variant="outline"
				disabled={busy || expired || disconnected || !bearer.trim()}
				onclick={() => void verifyBearer()}>Verify replacement key</Button
			>
		{/if}
	{/if}
	{#if verified}<p class="text-sm" role="status">
			Replacement Administrator verified for this plan.
		</p>{/if}
	<label class="flex items-start gap-2 text-sm"
		><input
			type="checkbox"
			class="size-4 shrink-0"
			bind:checked={confirmed}
			disabled={busy ||
				expired ||
				disconnected ||
				(plan.requiresAdministratorConfirmation && !verified)}
		/>{restore
			? 'I confirm replacing config.toml and secrets.toml on restart, including authentication settings.'
			: 'I confirm these authentication changes and understand that active browser sessions may be signed out.'}</label
	>
	<Button type="button" disabled={busy || !canApply} onclick={() => void apply()}
		>{busy
			? 'Working…'
			: restore
				? 'Confirm and stage restore'
				: 'Apply authentication changes'}</Button
	>
	{#if error}<p class="text-sm text-destructive" role="alert">{error}</p>{/if}
</section>
