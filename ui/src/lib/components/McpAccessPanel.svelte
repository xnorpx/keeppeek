<script lang="ts">
	import { onMount } from 'svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import {
		formatMcpEnabledDuration,
		type McpAccessControlClient
	} from '$lib/control-client-mcp-access';
	import type { McpAccessState } from '$lib/proto/webrtc_pb';
	import BellOffIcon from '@lucide/svelte/icons/bell-off';
	import BotIcon from '@lucide/svelte/icons/bot';
	import LoaderCircleIcon from '@lucide/svelte/icons/loader-circle';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import ShieldAlertIcon from '@lucide/svelte/icons/shield-alert';

	type Props = {
		controller: Pick<
			McpAccessControlClient,
			'getState' | 'setEnabled' | 'silenceLongEnabledWarning'
		> &
			Partial<Pick<McpAccessControlClient, 'onStateChange'>>;
		onstatechange?: (state: McpAccessState) => void;
	};

	let { controller, onstatechange }: Props = $props();
	const id = $props.id();

	let mcpState = $state.raw<McpAccessState | null>(null);
	let loading = $state(true);
	let busyAction = $state<'enable' | 'disable' | 'silence' | null>(null);
	let confirmingEnable = $state(false);
	let enableConfirmed = $state(false);
	let mustReload = $state(false);
	let error = $state<string | null>(null);
	let statusMessage = $state<string | null>(null);
	let loadGeneration = 0;
	let unmounted = false;

	let busy = $derived(loading || busyAction !== null);
	let enabledDurationLabel = $derived(
		mcpState?.enabled ? formatMcpEnabledDuration(mcpState.enabledDurationSecs) : null
	);
	let enabledSinceLabel = $derived.by(() => {
		const secs = mcpState?.enabledAtUnixSecs;
		if (secs === undefined || secs <= 0n) return null;
		const ms = Number(secs) * 1_000;
		return Number.isFinite(ms) ? new Date(ms).toLocaleString() : null;
	});
	let warningThresholdDays = $derived.by(() => {
		const threshold = Number(mcpState?.longEnabledWarningThresholdSecs ?? 604_800n);
		return Math.max(1, Math.round(threshold / 86_400));
	});

	onMount(() => {
		const unsubscribe = controller.onStateChange?.((next) => {
			if (unmounted) return;
			loadGeneration += 1;
			loading = false;
			applyState(next);
			error = null;
		});
		void loadState();
		return () => {
			unmounted = true;
			loadGeneration += 1;
			unsubscribe?.();
		};
	});

	function applyState(next: McpAccessState): void {
		mcpState = next;
		confirmingEnable = false;
		enableConfirmed = false;
		mustReload = false;
	}

	async function loadState(): Promise<void> {
		const version = ++loadGeneration;
		loading = true;
		error = null;
		statusMessage = null;
		try {
			const loaded = await controller.getState();
			if (version === loadGeneration && !unmounted) {
				applyState(loaded);
			}
		} catch (cause) {
			if (version === loadGeneration && !unmounted) {
				error =
					cause instanceof Error
						? cause.message
						: 'Unable to load MCP access settings. Reload to try again.';
			}
		} finally {
			if (version === loadGeneration && !unmounted) {
				loading = false;
			}
		}
	}

	async function handleConfirmEnable(): Promise<void> {
		if (!mcpState || busy || mustReload || !enableConfirmed || !mcpState.humanControlAllowed)
			return;
		busyAction = 'enable';
		error = null;
		statusMessage = null;
		try {
			const updated = await controller.setEnabled(true, true, mcpState.configurationRevision);
			if (unmounted) return;
			loadGeneration += 1;
			applyState(updated);
			statusMessage = 'MCP access enabled live on POST /mcp.';
			onstatechange?.(updated);
		} catch (cause) {
			if (unmounted) return;
			mustReload = true;
			error =
				cause instanceof Error
					? cause.message
					: 'Unable to enable MCP access. Reload settings and try again.';
		} finally {
			if (!unmounted) {
				busyAction = null;
			}
		}
	}

	async function handleDisable(): Promise<void> {
		if (!mcpState || busy || mustReload) return;
		busyAction = 'disable';
		error = null;
		statusMessage = null;
		try {
			const updated = await controller.setEnabled(false, false, mcpState.configurationRevision);
			if (unmounted) return;
			loadGeneration += 1;
			applyState(updated);
			statusMessage =
				'MCP access disabled live. POST /mcp and /mcp/secret-entry now return 404 immediately.';
			onstatechange?.(updated);
		} catch (cause) {
			if (unmounted) return;
			mustReload = true;
			error =
				cause instanceof Error
					? cause.message
					: 'Unable to disable MCP access. Reload settings and try again.';
		} finally {
			if (!unmounted) {
				busyAction = null;
			}
		}
	}

	async function handleSilenceWarning(): Promise<void> {
		if (!mcpState || busy || mustReload || !mcpState.humanControlAllowed) return;
		busyAction = 'silence';
		error = null;
		statusMessage = null;
		try {
			const updated = await controller.silenceLongEnabledWarning(mcpState.configurationRevision);
			if (unmounted) return;
			loadGeneration += 1;
			applyState(updated);
			statusMessage =
				'7-day MCP warning silenced for this enabled session. Disabling and re-enabling resets the timer.';
			onstatechange?.(updated);
		} catch (cause) {
			if (unmounted) return;
			mustReload = true;
			error =
				cause instanceof Error
					? cause.message
					: 'Unable to silence the MCP warning. Reload settings and try again.';
		} finally {
			if (!unmounted) {
				busyAction = null;
			}
		}
	}
</script>

<article
	id="mcp-access"
	class="grid gap-4 p-5 lg:grid-cols-[17rem_minmax(0,1fr)_17rem] lg:items-start"
	aria-labelledby="{id}-heading"
	data-mcp-access-panel
>
	<div class="space-y-3">
		<div class="flex items-center gap-2">
			<span
				class="grid size-8 place-items-center rounded-sm border border-hairline bg-raised text-primary-soft"
			>
				<BotIcon class="size-4" />
			</span>
			<div>
				<h3 id="{id}-heading" class="text-base font-semibold">MCP server</h3>
				<p
					class="font-mono text-2xs tracking-caps {mcpState?.longEnabledWarningActive
						? 'text-activity'
						: mcpState?.enabled
							? 'text-healthy'
							: 'text-text-faint'}"
					data-testid="mcp-access-status"
				>
					{#if loading}
						LOADING
					{:else if mcpState?.enabled}
						ENABLED
					{:else}
						DISABLED
					{/if}
				</p>
			</div>
		</div>

		{#if !loading && mcpState}
			<div class="flex flex-wrap gap-2">
				{#if mcpState.enabled}
					<Button
						type="button"
						variant="destructive"
						size="sm"
						disabled={busy || mustReload}
						onclick={() => void handleDisable()}
					>
						{busyAction === 'disable' ? 'Disabling…' : 'Disable MCP'}
					</Button>
				{:else if !confirmingEnable}
					<Button
						type="button"
						variant="outline"
						size="sm"
						disabled={busy || mustReload || !mcpState.humanControlAllowed}
						onclick={() => {
							confirmingEnable = true;
							enableConfirmed = false;
							error = null;
							statusMessage = null;
						}}
					>
						Enable MCP access
					</Button>
				{/if}
				{#if mustReload}
					<Button type="button" variant="outline" size="sm" onclick={() => void loadState()}>
						<RefreshCwIcon class="size-3.5" />
						Reload settings
					</Button>
				{/if}
			</div>
		{/if}
	</div>

	<div class="space-y-3">
		{#if loading}
			<div class="flex items-center gap-2 text-sm text-text-muted">
				<LoaderCircleIcon class="size-4 animate-spin" />
				Loading MCP access state…
			</div>
		{:else if mcpState}
			<div>
				<p class="font-mono text-2xs tracking-caps text-text-faint">GOVERNANCE BOUNDARY</p>
				<p class="mt-1 text-sm text-foreground">
					Embedded Streamable HTTP endpoint at <code class="font-mono text-xs">POST /mcp</code>.
					Only a human Administrator can enable MCP or silence the {warningThresholdDays}-day
					warning. Agents can turn MCP off themselves with
					<code class="font-mono text-xs">disable_mcp_access</code>, never enable it.
				</p>
			</div>

			{#if mcpState.enabled && enabledDurationLabel}
				<div class="grid gap-2 text-xs sm:grid-cols-2">
					<div class="rounded-sm border border-hairline bg-raised/50 px-3 py-2">
						<p class="font-mono text-2xs tracking-caps text-text-faint">ENABLED DURATION</p>
						<p class="mt-0.5 font-medium text-foreground" data-testid="mcp-enabled-duration">
							{enabledDurationLabel}
						</p>
					</div>
					{#if enabledSinceLabel}
						<div class="rounded-sm border border-hairline bg-raised/50 px-3 py-2">
							<p class="font-mono text-2xs tracking-caps text-text-faint">ENABLED SINCE</p>
							<p class="mt-0.5 font-medium text-foreground">{enabledSinceLabel}</p>
						</div>
					{/if}
				</div>
			{/if}

			{#if mcpState.longEnabledWarningActive}
				<div
					class="space-y-2 rounded-sm border border-amber-500/50 bg-amber-500/10 p-3 text-xs"
					role="region"
					aria-label="MCP enabled for more than {warningThresholdDays} days"
					data-testid="mcp-panel-long-enabled-warning"
				>
					<div class="flex items-center gap-2 font-medium text-foreground">
						<ShieldAlertIcon class="size-4 shrink-0 text-amber-500" />
						<span>
							MCP has been enabled for {enabledDurationLabel ?? `${warningThresholdDays}+ days`}.
							Disable it if no assistant is actively working on this recorder.
						</span>
					</div>
					<div class="flex flex-wrap gap-2">
						<Button
							type="button"
							variant="destructive"
							size="sm"
							disabled={busy || mustReload}
							onclick={() => void handleDisable()}
						>
							{busyAction === 'disable' ? 'Disabling…' : 'Disable MCP now'}
						</Button>
						<Button
							type="button"
							variant="outline"
							size="sm"
							disabled={busy || mustReload || !mcpState.humanControlAllowed}
							onclick={() => void handleSilenceWarning()}
						>
							<BellOffIcon class="size-3.5" />
							{busyAction === 'silence' ? 'Silencing…' : 'Silence warning'}
						</Button>
					</div>
				</div>
			{:else if mcpState.enabled && mcpState.longEnabledWarningSilenced}
				<p
					class="rounded-sm border border-hairline bg-raised/50 px-3 py-2 text-xs text-text-muted"
					data-testid="mcp-warning-silenced-note"
				>
					The {warningThresholdDays}-day enabled warning is silenced for this enabled session.
					Disabling and later re-enabling MCP starts a fresh {warningThresholdDays}-day timer.
				</p>
			{/if}

			{#if !mcpState.humanControlAllowed}
				<p
					class="rounded-sm border border-hairline bg-raised/50 px-3 py-2 text-xs text-text-muted"
					data-testid="mcp-human-control-required"
				>
					This session uses a Bearer access key. Enabling MCP or silencing the
					{warningThresholdDays}-day warning requires trusted local network access or an interactive
					OIDC/identity-proxy Administrator browser session.
				</p>
			{/if}

			{#if confirmingEnable && !mcpState.enabled}
				<div
					class="space-y-3 rounded-sm border border-amber-500/50 bg-amber-500/10 p-3 text-xs"
					role="region"
					aria-label="Confirm enabling MCP access"
					data-testid="mcp-enable-confirmation"
				>
					<p class="font-medium text-foreground">Confirm enabling MCP access live</p>
					<p class="text-text-muted">
						This immediately opens <code class="font-mono">POST /mcp</code> and loopback
						<code class="font-mono">/mcp/secret-entry</code> using your saved
						<code class="font-mono">[mcp]</code> policy limits. KeepPeek will warn Administrators if
						MCP remains enabled for {warningThresholdDays} days or longer.
					</p>
					<label for="{id}-confirm-enable" class="flex cursor-pointer items-start gap-2">
						<input
							id="{id}-confirm-enable"
							type="checkbox"
							class="mt-0.5 size-4"
							checked={enableConfirmed}
							onchange={(event) => (enableConfirmed = event.currentTarget.checked)}
						/>
						<span>I confirm I want to enable MCP access on this KeepPeek recorder.</span>
					</label>
					<div class="flex flex-wrap gap-2">
						<Button
							type="button"
							size="sm"
							disabled={busy || mustReload || !enableConfirmed}
							onclick={() => void handleConfirmEnable()}
						>
							{busyAction === 'enable' ? 'Enabling…' : 'Confirm enable MCP'}
						</Button>
						<Button
							type="button"
							variant="outline"
							size="sm"
							disabled={busy}
							onclick={() => {
								confirmingEnable = false;
								enableConfirmed = false;
							}}
						>
							Cancel
						</Button>
					</div>
				</div>
			{/if}

			{#if statusMessage}
				<p class="text-xs text-text-muted" role="status" aria-live="polite">{statusMessage}</p>
			{/if}
			{#if error}
				<p class="text-xs text-destructive" role="alert">{error}</p>
			{/if}
		{:else if error}
			<p class="text-xs text-destructive" role="alert">{error}</p>
			<Button type="button" variant="outline" size="sm" onclick={() => void loadState()}>
				<RefreshCwIcon class="size-3.5" />
				Retry
			</Button>
		{/if}
	</div>

	<div class="rounded-sm border border-hairline bg-raised/40 px-3 py-3 text-xs">
		<p class="font-mono text-2xs tracking-caps text-text-faint">SAVED POLICY FLAGS</p>
		{#if mcpState}
			<dl class="mt-2 space-y-1.5">
				<div class="flex justify-between gap-2">
					<dt class="text-text-muted">Config writes</dt>
					<dd class="font-mono">{mcpState.allowWrites ? 'allowed' : 'off'}</dd>
				</div>
				<div class="flex justify-between gap-2">
					<dt class="text-text-muted">Snapshots</dt>
					<dd class="font-mono">{mcpState.allowSnapshots ? 'allowed' : 'off'}</dd>
				</div>
				<div class="flex justify-between gap-2">
					<dt class="text-text-muted">Camera writes</dt>
					<dd class="font-mono">{mcpState.allowCameraDeviceWrites ? 'allowed' : 'off'}</dd>
				</div>
			</dl>
			<p class="mt-2 text-[11px] leading-4 text-text-faint">
				Policy flags and payload caps are configured in <code class="font-mono">config.toml</code>
				under <code class="font-mono">[mcp]</code>.
			</p>
		{:else}
			<p class="mt-2 text-text-muted">Policy flags load with MCP access state.</p>
		{/if}
	</div>
</article>
