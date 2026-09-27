<script lang="ts">
	import type { AccessCredential } from '$lib/access';
	import { AccessRole, type ExternalIdentity } from '$lib/proto/webrtc_pb';
	import type { PeekLayoutAudience } from '$lib/peek-layout';

	type Props = {
		credentials: readonly AccessCredential[];
		externalIdentities?: readonly ExternalIdentity[];
		audience: PeekLayoutAudience;
		onchange: (audience: PeekLayoutAudience) => void;
	};

	let { credentials, externalIdentities = [], audience, onchange }: Props = $props();
	let externalUsers = $derived(
		externalIdentities
			.filter((identity) => identity.role === AccessRole.USER)
			.toSorted((left, right) => left.displayName.localeCompare(right.displayName))
	);
	let userCredentials = $derived(
		credentials
			.filter((credential) => credential.role === 'user' && credential.revokedAtMs === null)
			.toSorted((left, right) => left.name.localeCompare(right.name))
	);

	function setEveryone(event: Event): void {
		if (!(event.currentTarget instanceof HTMLInputElement)) return;
		onchange({ everyone: event.currentTarget.checked, credentialIds: [] });
	}

	function setCredential(credentialId: string, event: Event): void {
		if (!(event.currentTarget instanceof HTMLInputElement)) return;
		const selected = new Set(audience.credentialIds);
		if (event.currentTarget.checked) selected.add(credentialId);
		else selected.delete(credentialId);
		onchange({ everyone: false, credentialIds: [...selected].toSorted() });
	}

	function status(credential: AccessCredential): string {
		if (credential.disabled) return 'Disabled';
		if (credential.expiresAtMs !== null && credential.expiresAtMs <= Date.now()) return 'Expired';
		return 'Active';
	}
</script>

<fieldset class="space-y-3" aria-label="Dashboard viewers">
	<legend class="text-xs font-semibold">Who can view</legend>
	<label
		class="flex min-h-10 items-center gap-3 rounded-sm border border-hairline bg-raised px-3 text-xs"
	>
		<input type="checkbox" class="size-4" checked={audience.everyone} onchange={setEveryone} />
		<span class="min-w-0 flex-1">
			<span class="block font-medium">Everyone with KeepPeek access</span>
			<span class="block text-text-muted">All named credentials and external identities</span>
		</span>
	</label>

	{#if !audience.everyone}
		<div class="max-h-48 divide-y divide-hairline overflow-y-auto border-y border-hairline">
			{#each userCredentials as credential (credential.id)}
				<label class="flex min-h-11 items-center gap-3 px-3 text-xs">
					<input
						type="checkbox"
						class="size-4"
						checked={audience.credentialIds.includes(credential.id)}
						disabled={status(credential) !== 'Active'}
						onchange={(event) => setCredential(credential.id, event)}
					/>
					<span class="min-w-0 flex-1 truncate font-medium">{credential.name}</span>
					<span class="shrink-0 font-mono text-2xs text-text-faint">{status(credential)}</span>
				</label>
			{/each}
			{#each externalUsers as identity (identity.identityId)}
				<label class="flex min-h-11 items-center gap-3 px-3 text-xs">
					<input
						type="checkbox"
						class="size-4"
						checked={audience.credentialIds.includes(identity.identityId)}
						disabled={!identity.enabled}
						onchange={(event) => setCredential(identity.identityId, event)}
					/>
					<span class="min-w-0 flex-1"
						><span class="block truncate font-medium">{identity.displayName}</span><span
							class="block text-text-muted">{identity.providerName} · External identity</span
						></span
					>
					<span class="text-2xs text-text-faint">{identity.enabled ? 'Active' : 'Revoked'}</span>
				</label>
			{/each}
			{#if !userCredentials.length && !externalUsers.length}<p
					class="px-3 py-4 text-xs text-text-muted"
				>
					No User identities are available.
				</p>{/if}
		</div>
	{/if}

	<p class="text-xs text-text-muted">Administrators always have access.</p>
</fieldset>
