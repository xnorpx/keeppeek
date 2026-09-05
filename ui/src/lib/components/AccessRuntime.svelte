<script lang="ts">
	import type {
		AccessAuditEvent,
		AccessCredential,
		AccessRole,
		AccessSession,
		IssuedAccessCredential
	} from '$lib/access';
	import type { ControlClient } from '$lib/control-client';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import CheckIcon from '@lucide/svelte/icons/check';
	import CopyIcon from '@lucide/svelte/icons/copy';
	import KeyRoundIcon from '@lucide/svelte/icons/key-round';
	import PlusIcon from '@lucide/svelte/icons/plus';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import RotateCwIcon from '@lucide/svelte/icons/rotate-cw';
	import ShieldOffIcon from '@lucide/svelte/icons/shield-off';
	import Trash2Icon from '@lucide/svelte/icons/trash-2';
	import { onMount } from 'svelte';
	import InitialAccessKeyClaim from './InitialAccessKeyClaim.svelte';
	import CameraAccessDialog from './CameraAccessDialog.svelte';
	import UserRoundIcon from '@lucide/svelte/icons/user-round';
	import { cameraAccessCapability } from '$lib/control-client-camera-access';

	type Props = {
		controller: ControlClient;
		onrevealaccesskey?: () => Promise<string>;
	};

	let { controller, onrevealaccesskey }: Props = $props();
	let credentials = $state.raw<AccessCredential[]>([]);
	let sessions = $state.raw<AccessSession[]>([]);
	let audit = $state.raw<AccessAuditEvent[]>([]);
	let loading = $state(true);
	let busyId = $state<string | null>(null);
	let error = $state<string | null>(null);
	let createOpen = $state(false);
	let name = $state('');
	let description = $state('');
	let role = $state<AccessRole>('user');
	let expiresInDays = $state('');
	let issued = $state.raw<IssuedAccessCredential | null>(null);
	let copied = $state(false);
	let pendingRevoke = $state<string | null>(null);
	let editingCameraAccess = $state.raw<AccessCredential | null>(null);
	let cameraAccessAvailable = $state(false);

	onMount(() => {
		void refresh();
		return controller.onCapabilities((ids) => {
			cameraAccessAvailable = ids.includes(cameraAccessCapability);
		});
	});

	async function refresh(): Promise<void> {
		loading = true;
		error = null;
		try {
			[credentials, sessions, audit] = await Promise.all([
				controller.listAccessCredentials(),
				controller.listAccessSessions(),
				controller.listAccessAudit(40)
			]);
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Access records could not be loaded.';
		} finally {
			loading = false;
		}
	}

	async function createCredential(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (!name.trim()) return;
		busyId = 'create';
		error = null;
		try {
			const days = expiresInDays ? Number(expiresInDays) : null;
			issued = await controller.createAccessCredential({
				name,
				description: description || undefined,
				role,
				expiresAtMs:
					days === null ? undefined : Date.now() + Math.round(days * 24 * 60 * 60 * 1_000)
			});
			name = '';
			description = '';
			expiresInDays = '';
			createOpen = false;
			await refresh();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Credential could not be created.';
		} finally {
			busyId = null;
		}
	}

	async function rotateCredential(credential: AccessCredential): Promise<void> {
		busyId = credential.id;
		error = null;
		try {
			issued = await controller.rotateAccessCredential(credential.id);
			await refresh();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Credential could not be rotated.';
		} finally {
			busyId = null;
		}
	}

	async function toggleCredential(credential: AccessCredential): Promise<void> {
		busyId = credential.id;
		error = null;
		try {
			await controller.setAccessCredentialEnabled(credential.id, credential.disabled);
			await refresh();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Credential could not be changed.';
		} finally {
			busyId = null;
		}
	}

	async function revokeCredential(credential: AccessCredential): Promise<void> {
		if (pendingRevoke !== credential.id) {
			pendingRevoke = credential.id;
			return;
		}
		busyId = credential.id;
		error = null;
		try {
			await controller.revokeAccessCredential(credential.id);
			pendingRevoke = null;
			await refresh();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Credential could not be revoked.';
		} finally {
			busyId = null;
		}
	}

	async function revokeSession(session: AccessSession): Promise<void> {
		busyId = session.id;
		error = null;
		try {
			await controller.revokeAccessSession(session.id);
			await refresh();
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Session could not be revoked.';
		} finally {
			busyId = null;
		}
	}

	async function copyIssuedKey(): Promise<void> {
		if (!issued) return;
		try {
			await navigator.clipboard.writeText(issued.accessKey);
			copied = true;
		} catch {
			error = 'Clipboard access is unavailable.';
		}
	}

	function hideIssuedKey(): void {
		issued = null;
		copied = false;
	}

	function markInitialAccessKeyClaimed(): void {
		credentials = credentials.map((credential) =>
			credential.initialAccessKeyPending
				? { ...credential, initialAccessKeyPending: false }
				: credential
		);
	}

	function credentialStatus(credential: AccessCredential): string {
		if (credential.revokedAtMs !== null) return 'Revoked';
		if (credential.disabled) return 'Disabled';
		if (credential.expiresAtMs !== null && credential.expiresAtMs <= Date.now()) return 'Expired';
		return 'Active';
	}

	function formatTimestamp(timestampMs: number | null): string {
		return timestampMs === null ? 'Never' : new Date(timestampMs).toLocaleString();
	}

	function shortId(value: string): string {
		return value.length > 14 ? `${value.slice(0, 8)}…${value.slice(-4)}` : value;
	}
</script>

{#if onrevealaccesskey && credentials.some((credential) => credential.initialAccessKeyPending)}
	<div class="border-b border-hairline p-5">
		<InitialAccessKeyClaim
			pending
			onclaim={onrevealaccesskey}
			onclaimed={markInitialAccessKeyClaimed}
		/>
	</div>
{/if}

<section class="border-b border-hairline" aria-labelledby="credentials-heading">
	<header class="flex flex-wrap items-center justify-between gap-3 px-5 py-4">
		<div>
			<h3 id="credentials-heading" class="text-base font-semibold">Access credentials</h3>
			<p class="mt-1 text-xs text-text-muted">{credentials.length} durable identities</p>
		</div>
		<div class="flex gap-2">
			<Button
				variant="outline"
				size="sm"
				onclick={() => void refresh()}
				disabled={loading}
				title="Refresh access records"
			>
				<RefreshCwIcon class="size-3.5" /> Refresh
			</Button>
			<Button data-new-access-credential size="sm" onclick={() => (createOpen = !createOpen)}>
				<PlusIcon class="size-3.5" /> New credential
			</Button>
		</div>
	</header>

	{#if createOpen}
		<form
			class="grid gap-3 border-t border-hairline bg-raised/30 px-5 py-4 md:grid-cols-2"
			onsubmit={(event) => void createCredential(event)}
		>
			<label class="space-y-1 text-xs font-medium">
				<span>Name</span>
				<Input bind:value={name} maxlength={64} required />
			</label>
			<label class="space-y-1 text-xs font-medium">
				<span>Description</span>
				<Input bind:value={description} maxlength={256} />
			</label>
			<label class="space-y-1 text-xs font-medium">
				<span>Role</span>
				<select
					bind:value={role}
					class="h-9 w-full rounded-sm border border-input bg-background px-3 text-sm focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				>
					<option value="user">User</option>
					<option value="administrator">Administrator</option>
				</select>
			</label>
			<label class="space-y-1 text-xs font-medium">
				<span>Expires in days</span>
				<Input type="number" bind:value={expiresInDays} min="1" max="3650" step="1" />
			</label>
			<div class="flex gap-2 md:col-span-2">
				<Button type="submit" size="sm" disabled={busyId === 'create' || !name.trim()}>
					<KeyRoundIcon class="size-3.5" />
					{busyId === 'create' ? 'Creating' : 'Create'}
				</Button>
				<Button type="button" variant="ghost" size="sm" onclick={() => (createOpen = false)}
					>Cancel</Button
				>
			</div>
		</form>
	{/if}

	{#if issued}
		<div class="border-t border-healthy/40 bg-healthy/5 px-5 py-4" data-issued-access-key>
			<div class="flex flex-wrap items-start justify-between gap-3">
				<div>
					<p class="text-sm font-semibold">{issued.credential.name} is ready</p>
					<p class="mt-1 text-xs text-text-muted">
						This access key is shown only for this operation.
					</p>
				</div>
				<div class="flex gap-2">
					<Button variant="outline" size="sm" onclick={() => void copyIssuedKey()}>
						{#if copied}<CheckIcon class="size-3.5" /> Copied{:else}<CopyIcon class="size-3.5" /> Copy{/if}
					</Button>
					<Button variant="ghost" size="sm" onclick={hideIssuedKey}>Hide</Button>
				</div>
			</div>
			<code
				class="mt-3 block overflow-x-auto border-y border-hairline bg-background px-3 py-2 font-mono text-xs select-all"
				>{issued.accessKey}</code
			>
		</div>
	{/if}

	<div class="overflow-x-auto border-t border-hairline">
		<table class="w-full min-w-[52rem] text-left text-xs">
			<thead class="bg-raised text-text-faint">
				<tr>
					<th class="px-5 py-2 font-mono text-2xs">IDENTITY</th>
					<th class="px-3 py-2 font-mono text-2xs">ROLE</th>
					<th class="px-3 py-2 font-mono text-2xs">STATUS</th>
					<th class="px-3 py-2 font-mono text-2xs">LAST USED</th>
					<th class="px-3 py-2 font-mono text-2xs">EXPIRES</th>
					<th class="w-40 px-5 py-2 text-right font-mono text-2xs">ACTIONS</th>
				</tr>
			</thead>
			<tbody class="divide-y divide-hairline">
				{#each credentials as credential (credential.id)}
					<tr>
						<td class="px-5 py-3">
							<p class="font-medium">{credential.name}</p>
							<p class="mt-0.5 text-text-faint">
								{credential.description ?? shortId(credential.id)}
							</p>
						</td>
						<td class="px-3 py-3 capitalize">{credential.role}</td>
						<td class="px-3 py-3">{credentialStatus(credential)}</td>
						<td class="px-3 py-3 text-text-muted">{formatTimestamp(credential.lastUsedAtMs)}</td>
						<td class="px-3 py-3 text-text-muted">{formatTimestamp(credential.expiresAtMs)}</td>
						<td class="px-5 py-3">
							<div class="flex justify-end gap-1">
								{#if cameraAccessAvailable && credential.role === 'user'}
									<Button
										variant="ghost"
										size="icon-sm"
										onclick={() => (editingCameraAccess = credential)}
										disabled={busyId === credential.id || credential.revokedAtMs !== null}
										aria-label={`User access for ${credential.name}`}
										title="User access"><UserRoundIcon class="size-3.5" /></Button
									>
								{/if}
								<Button
									variant="ghost"
									size="icon-sm"
									onclick={() => void rotateCredential(credential)}
									disabled={busyId === credential.id || credential.revokedAtMs !== null}
									title="Rotate credential"><RotateCwIcon class="size-3.5" /></Button
								>
								<Button
									variant="ghost"
									size="icon-sm"
									onclick={() => void toggleCredential(credential)}
									disabled={busyId === credential.id || credential.revokedAtMs !== null}
									title={credential.disabled ? 'Enable credential' : 'Disable credential'}
									><ShieldOffIcon class="size-3.5" /></Button
								>
								<Button
									variant={pendingRevoke === credential.id ? 'destructive' : 'ghost'}
									size="icon-sm"
									onclick={() => void revokeCredential(credential)}
									disabled={busyId === credential.id || credential.revokedAtMs !== null}
									title={pendingRevoke === credential.id ? 'Confirm revoke' : 'Revoke credential'}
									><Trash2Icon class="size-3.5" /></Button
								>
							</div>
						</td>
					</tr>
				{/each}
				{#if !loading && credentials.length === 0}
					<tr
						><td colspan="6" class="px-5 py-8 text-center text-text-muted">No access credentials</td
						></tr
					>
				{/if}
			</tbody>
		</table>
	</div>
</section>

<div class="grid lg:grid-cols-2">
	<section
		class="border-b border-hairline p-5 lg:border-r lg:border-b-0"
		aria-labelledby="sessions-heading"
	>
		<h3 id="sessions-heading" class="text-base font-semibold">Active sessions</h3>
		<p class="mt-1 text-xs text-text-muted">{sessions.length} connected</p>
		<div class="mt-4 divide-y divide-hairline border-y border-hairline">
			{#each sessions as session (session.id)}
				<div class="flex items-center gap-3 py-3">
					<div class="min-w-0 flex-1">
						<p class="truncate text-sm font-medium">{session.displayName}</p>
						<p class="mt-0.5 text-xs text-text-faint">
							{session.local ? 'Local' : 'Remote'} · {session.clientClassification} · {shortId(
								session.id
							)}
						</p>
					</div>
					<Button
						variant="ghost"
						size="icon-sm"
						onclick={() => void revokeSession(session)}
						disabled={busyId === session.id}
						title="Revoke session"><Trash2Icon class="size-3.5" /></Button
					>
				</div>
			{/each}
			{#if !loading && sessions.length === 0}<p class="py-6 text-center text-xs text-text-muted">
					No active sessions
				</p>{/if}
		</div>
	</section>

	<section class="p-5" aria-labelledby="audit-heading">
		<h3 id="audit-heading" class="text-base font-semibold">Security audit</h3>
		<p class="mt-1 text-xs text-text-muted">Latest bounded access events</p>
		<div class="mt-4 max-h-72 overflow-auto border-y border-hairline">
			{#each audit.toReversed() as event (event.id)}
				<div
					class="grid grid-cols-[minmax(0,1fr)_auto] gap-3 border-b border-hairline py-2.5 last:border-b-0"
				>
					<div class="min-w-0">
						<p class="truncate text-xs font-medium">{event.action.replaceAll('_', ' ')}</p>
						<p class="mt-0.5 truncate text-2xs text-text-faint">
							{event.result} · {event.clientClassification}
						</p>
					</div>
					<time
						class="text-2xs text-text-faint"
						datetime={new Date(event.timestampMs).toISOString()}
						>{formatTimestamp(event.timestampMs)}</time
					>
				</div>
			{/each}
			{#if !loading && audit.length === 0}<p class="py-6 text-center text-xs text-text-muted">
					No audit events
				</p>{/if}
		</div>
	</section>
</div>

{#if error}
	<p class="border-t border-hairline px-5 py-3 text-xs text-destructive" role="alert">{error}</p>
{/if}

{#if editingCameraAccess}
	<CameraAccessDialog
		credential={editingCameraAccess}
		{controller}
		onclose={() => (editingCameraAccess = null)}
		onsaved={() => void refresh()}
	/>
{/if}
