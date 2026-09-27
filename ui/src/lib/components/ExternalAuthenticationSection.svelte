<script lang="ts">
	import { clone, toJsonString } from '@bufbuild/protobuf';
	import { onMount } from 'svelte';
	import type { ControlClient } from '$lib/control-client';
	import {
		AccessRole,
		ExternalAuthenticationSettingsSchema,
		type ConfigurationPlan,
		type ExternalIdentity,
		type BrowserSession
	} from '$lib/proto/webrtc_pb';
	import {
		newExternalSettings,
		newExternalProvider,
		textList
	} from '$lib/external-authentication-admin';
	import ExternalAuthenticationProviderForm from './ExternalAuthenticationProviderForm.svelte';
	import ExternalAuthenticationPlan from './ExternalAuthenticationPlan.svelte';
	import { Button } from './ui/button/index.js';
	import { Input } from './ui/input/index.js';
	type Controller = Pick<
		ControlClient,
		| 'getExternalAuthentication'
		| 'listExternalIdentities'
		| 'listBrowserSessions'
		| 'planExternalAuthentication'
		| 'revokeExternalIdentity'
		| 'revokeBrowserSession'
		| 'onAccessState'
		| 'prepareAdministratorVerification'
		| 'getAdministratorVerification'
		| 'verifyAdministratorBearer'
		| 'applyExternalAuthentication'
	>;
	let { controller }: { controller: Controller } = $props();
	let settings = $state(newExternalSettings());
	let enabled = $state(false);
	let revision = $state('');
	let busy = $state(true);
	let error = $state<string | null>(null);
	let message = $state('');
	let plan = $state.raw<ConfigurationPlan | null>(null);
	let plannedDraft = $state('');
	let identities = $state.raw<ExternalIdentity[]>([]);
	let sessions = $state.raw<BrowserSession[]>([]);
	let identityPage = $state('');
	let sessionPage = $state('');
	let pendingRevoke = $state('');
	let alive = true;
	let now = $state(Date.now());
	let draft = $derived(
		enabled ? toJsonString(ExternalAuthenticationSettingsSchema, settings) : 'disabled'
	);

	onMount(() => {
		void load();
		const clock = setInterval(() => (now = Date.now()), 60_000);
		return () => {
			alive = false;
			clearInterval(clock);
		};
	});
	async function load(): Promise<void> {
		busy = true;
		error = null;
		plan = null;
		try {
			const [config, people, browsers] = await Promise.all([
				controller.getExternalAuthentication(),
				controller.listExternalIdentities(),
				controller.listBrowserSessions()
			]);
			if (!alive) return;
			revision = config.configurationRevision;
			enabled = Boolean(config.settings);
			settings = config.settings
				? clone(ExternalAuthenticationSettingsSchema, config.settings)
				: newExternalSettings();
			identities = people.identities;
			identityPage = people.nextPageToken;
			sessions = browsers.sessions;
			sessionPage = browsers.nextPageToken;
		} catch {
			if (alive)
				error =
					'External authentication settings are unavailable. Check Administrator access and retry.';
		} finally {
			if (alive) busy = false;
		}
	}
	async function preview(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		busy = true;
		error = null;
		message = '';
		plan = null;
		const candidate = draft;
		try {
			const next = await controller.planExternalAuthentication(
				enabled ? clone(ExternalAuthenticationSettingsSchema, settings) : null,
				revision
			);
			if (alive && candidate === draft) {
				plannedDraft = candidate;
				plan = next;
			}
		} catch (cause) {
			if (alive) error = cause instanceof Error ? cause.message : 'Plan validation failed.';
		} finally {
			if (alive) busy = false;
		}
	}
	async function more(kind: 'identities' | 'sessions'): Promise<void> {
		busy = true;
		error = null;
		try {
			if (kind === 'identities') {
				const next = await controller.listExternalIdentities(identityPage);
				if (alive) {
					identities = next.identities;
					identityPage = next.nextPageToken;
				}
			} else {
				const next = await controller.listBrowserSessions(sessionPage);
				if (alive) {
					sessions = next.sessions;
					sessionPage = next.nextPageToken;
				}
			}
		} catch {
			if (alive) error = 'The next page could not be loaded. Retry.';
		} finally {
			if (alive) busy = false;
		}
	}
	async function revoke(
		kind: 'identity' | 'session',
		id: string,
		expectedRevision = 0n
	): Promise<void> {
		if (pendingRevoke !== `${kind}:${id}`) {
			pendingRevoke = `${kind}:${id}`;
			return;
		}
		busy = true;
		error = null;
		try {
			if (kind === 'identity') await controller.revokeExternalIdentity(id, expectedRevision);
			else await controller.revokeBrowserSession(id);
			const [people, browsers] = await Promise.all([
				controller.listExternalIdentities(),
				controller.listBrowserSessions()
			]);
			if (alive) {
				identities = people.identities;
				identityPage = people.nextPageToken;
				sessions = browsers.sessions;
				sessionPage = browsers.nextPageToken;
				plan = null;
				message =
					'Access revoked. The settings draft is unchanged; reload settings if its revision has changed.';
			}
		} catch {
			if (alive)
				error =
					'Revocation failed. Refresh the directory and check that Administrator access is retained.';
		} finally {
			if (alive) {
				busy = false;
				pendingRevoke = '';
			}
		}
	}
	function transition(value: string): void {
		const milliseconds = value ? new Date(value).getTime() : NaN;
		settings.bearerTransitionUntilMs = Number.isFinite(milliseconds)
			? BigInt(milliseconds)
			: undefined;
	}
</script>

<section
	class="space-y-5 border-b border-hairline p-5"
	aria-labelledby="external-authentication-heading"
	aria-busy={busy}
>
	<div class="flex flex-wrap items-center justify-between gap-3">
		<h3 id="external-authentication-heading" class="text-base font-semibold">External sign-in</h3>
		<Button variant="outline" disabled={busy} onclick={() => void load()}
			>Reload authentication settings</Button
		>
	</div>
	<p class="text-sm text-text-muted">
		Configure OpenID Connect or a trusted identity proxy. Unmapped accounts are denied. Camera
		grants are enforced by the recorder.
	</p>
	{#if error}<p role="alert" class="text-sm text-destructive">{error}</p>{/if}
	{#if message}<p role="status" class="text-sm">{message}</p>{/if}
	{#if revision}
		<form class="space-y-4" onsubmit={(event) => void preview(event)}>
			<fieldset disabled={busy} class="space-y-4">
				<label class="flex items-center gap-2 text-sm"
					><input type="checkbox" bind:checked={enabled} />Enable external sign-in</label
				>
				{#if enabled}
					<label class="block space-y-1 text-xs"
						>Allowed recorder origins (comma separated)<Input
							value={settings.allowedOrigins.join(', ')}
							oninput={(event) => (settings.allowedOrigins = textList(event.currentTarget.value))}
							placeholder="https://recorder.example"
							required
						/></label
					>
					{#each settings.providers as provider, index (index)}<ExternalAuthenticationProviderForm
							bind:provider={settings.providers[index]}
							{index}
							onremove={() =>
								(settings.providers = settings.providers.filter((_, i) => i !== index))}
						/>{/each}
					<Button
						type="button"
						variant="outline"
						disabled={settings.providers.length >= 4}
						onclick={() => settings.providers.push(newExternalProvider())}>Add provider</Button
					>
					<label class="flex items-center gap-2 text-sm"
						><input
							type="checkbox"
							checked={settings.bearerEnabled}
							onchange={(event) => {
								settings.bearerEnabled = event.currentTarget.checked;
								if (!settings.bearerEnabled) settings.bearerTransitionUntilMs = undefined;
							}}
						/>Allow bearer access during a limited transition</label
					>
					{#if settings.bearerEnabled}<label class="block space-y-1 text-xs"
							>Bearer transition deadline (UTC)<Input
								type="datetime-local"
								value={settings.bearerTransitionUntilMs
									? new Date(Number(settings.bearerTransitionUntilMs)).toISOString().slice(0, 16)
									: ''}
								oninput={(event) =>
									transition(`${event.currentTarget.value}${event.currentTarget.value ? 'Z' : ''}`)}
								required
							/></label
						>{/if}
				{/if}
				<Button type="submit">Preview authentication changes</Button>
			</fieldset>
		</form>
	{/if}
	{#if plan && plannedDraft === draft}<ExternalAuthenticationPlan
			{controller}
			{plan}
			settings={enabled ? settings : null}
			onapplied={() => {
				plan = null;
				revision = '';
				message = 'Authentication settings applied. Browser sessions may need to sign in again.';
			}}
		/>{/if}
	<div class="grid gap-5 lg:grid-cols-2">
		<section class="space-y-3" aria-label="External identities">
			<h4 class="text-sm font-semibold">External identities</h4>
			{#each identities as identity (identity.identityId)}<div
					class="space-y-2 border-b border-hairline py-3"
				>
					<p class="text-sm">
						{identity.displayName} · {identity.providerName} · {identity.role ===
						AccessRole.ADMINISTRATOR
							? 'Administrator'
							: 'User'} · {identity.enabled ? 'Active' : 'Revoked'}
					</p>
					<p class="font-mono text-xs break-all text-text-muted">
						Subject fingerprint: {identity.subjectFingerprint}
					</p>
					<Button
						size="sm"
						variant="outline"
						disabled={busy || !identity.enabled}
						onclick={() => void revoke('identity', identity.identityId, identity.revision)}
						>{pendingRevoke === `identity:${identity.identityId}`
							? 'Confirm revoke identity'
							: 'Revoke identity'}
						{identity.displayName}</Button
					>
				</div>{:else}<p class="text-xs text-text-muted">
					No external identities on this page.
				</p>{/each}
			{#if identityPage}<Button
					variant="outline"
					disabled={busy}
					onclick={() => void more('identities')}>Next identities</Button
				>{/if}
		</section>
		<section class="space-y-3" aria-label="Browser sign-in sessions">
			<h4 class="text-sm font-semibold">Browser sign-in sessions</h4>
			{#each sessions as session (session.sessionId)}<div
					class="space-y-2 border-b border-hairline py-3"
				>
					<p class="text-xs break-all">
						{identities.find((identity) => identity.identityId === session.identityId)
							?.displayName ?? session.identityId}
					</p>
					<p class="text-xs text-text-muted">
						Session age: {Math.max(0, Math.floor((now - Number(session.createdAtMs)) / 60_000))} min
					</p>
					<p class="text-xs text-text-muted">
						Expires {new Date(Number(session.absoluteExpiresAtMs)).toLocaleString()}
					</p>
					<Button
						size="sm"
						variant="outline"
						disabled={busy}
						onclick={() => void revoke('session', session.sessionId)}
						>{pendingRevoke === `session:${session.sessionId}`
							? 'Confirm revoke session'
							: 'Revoke browser session'}</Button
					>
				</div>{:else}<p class="text-xs text-text-muted">No browser sessions on this page.</p>{/each}
			{#if sessionPage}<Button
					variant="outline"
					disabled={busy}
					onclick={() => void more('sessions')}>Next browser sessions</Button
				>{/if}
		</section>
	</div>
</section>
