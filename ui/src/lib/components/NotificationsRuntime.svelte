<script lang="ts">
	import { resolve } from '$app/paths';
	import { onMount } from 'svelte';
	import { useControlClient } from '$lib/control-context';
	import { NotificationConflictError } from '$lib/control-client';
	import {
		createNotificationRule,
		createPushoverConfig,
		type NotificationChannel,
		type NotificationHistoryGroup,
		type NotificationInbox,
		type NotificationItem,
		type NotificationRuleDefinition,
		type NotificationRuleRecord,
		type NotificationTrigger,
		type PushoverPublicConfig
	} from '$lib/notifications';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import BellIcon from '@lucide/svelte/icons/bell';
	import CheckIcon from '@lucide/svelte/icons/check';
	import CheckCheckIcon from '@lucide/svelte/icons/check-check';
	import ChevronDownIcon from '@lucide/svelte/icons/chevron-down';
	import ChevronUpIcon from '@lucide/svelte/icons/chevron-up';
	import CircleAlertIcon from '@lucide/svelte/icons/circle-alert';
	import CopyIcon from '@lucide/svelte/icons/copy';
	import ExternalLinkIcon from '@lucide/svelte/icons/external-link';
	import EyeIcon from '@lucide/svelte/icons/eye';
	import FlaskConicalIcon from '@lucide/svelte/icons/flask-conical';
	import HistoryIcon from '@lucide/svelte/icons/history';
	import InboxIcon from '@lucide/svelte/icons/inbox';
	import PencilIcon from '@lucide/svelte/icons/pencil';
	import PlusIcon from '@lucide/svelte/icons/plus';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import SaveIcon from '@lucide/svelte/icons/save';
	import TrashIcon from '@lucide/svelte/icons/trash-2';
	import XIcon from '@lucide/svelte/icons/x';
	import PushoverActionFields from './PushoverActionFields.svelte';

	type View = 'rules' | 'inbox' | 'history';
	type NotificationAction = NotificationRuleDefinition['actions'][number];
	type PushoverSecret = 'application_token' | 'user_key';
	type PushoverDestination = PushoverPublicConfig & {
		application_token: string;
		user_key: string;
	};
	const controlClient = useControlClient();
	const allWeekdays = [
		'monday',
		'tuesday',
		'wednesday',
		'thursday',
		'friday',
		'saturday',
		'sunday'
	] as const;
	const triggerOptions: Array<{ value: NotificationTrigger; label: string }> = [
		{ value: 'event_created', label: 'Event starts' },
		{ value: 'event_updated', label: 'Event enriched' },
		{ value: 'event_ended', label: 'Event ends' },
		{ value: 'outage_started', label: 'Camera outage' },
		{ value: 'recovery', label: 'Camera recovery' },
		{ value: 'storage_health', label: 'Storage health' },
		{ value: 'recording_health', label: 'Recording health' }
	];

	let view = $state<View>('rules');
	let rules = $state.raw<NotificationRuleRecord[]>([]);
	let inbox = $state.raw<NotificationInbox>({ items: [], unreadCount: 0n });
	let history = $state.raw<NotificationHistoryGroup[]>([]);
	let loading = $state(true);
	let refreshing = $state(false);
	let error = $state<string | null>(null);
	let status = $state<string | null>(null);
	let editorDraft = $state<NotificationRuleDefinition | null>(null);
	let editorRecord = $state.raw<NotificationRuleRecord | null>(null);
	let editorBusy = $state(false);
	let conflict = $state<string | null>(null);
	let pendingDeleteId = $state<string | null>(null);
	let pendingClearAll = $state(false);
	let browserPermission = $state<NotificationPermission | 'unsupported'>('unsupported');
	let knownRevisions = new Map<string, bigint>();
	let initialInboxLoaded = false;
	let ruleCount = $derived(rules.length);
	let activeRuleCount = $derived(rules.filter((record) => record.active?.enabled).length);
	let suppressionCount = $derived(
		history.reduce(
			(total, group) =>
				total +
				group.events.filter((event) =>
					['suppressed', 'collapsed', 'rate_limited', 'expired'].includes(event.outcome)
				).length,
			0
		)
	);

	onMount(() => {
		browserPermission = 'Notification' in window ? Notification.permission : 'unsupported';
		void reload(true);
		const interval = window.setInterval(() => void reload(false), 5_000);
		return () => window.clearInterval(interval);
	});

	async function reload(showLoading: boolean): Promise<void> {
		if (refreshing) return;
		refreshing = true;
		if (showLoading) loading = true;
		try {
			const [nextRules, nextInbox, nextHistory] = await Promise.all([
				controlClient.listNotificationRules(),
				controlClient.getNotificationInbox(),
				controlClient.getNotificationHistory()
			]);
			rules = nextRules;
			publishBrowserUpdates(nextInbox);
			inbox = nextInbox;
			history = nextHistory;
			error = null;
		} catch (cause) {
			error = message(cause, 'Unable to load notifications.');
		} finally {
			refreshing = false;
			loading = false;
		}
	}

	function publishBrowserUpdates(nextInbox: NotificationInbox): void {
		const nextRevisions = new Map(
			nextInbox.items.map((item) => [item.logicalId, item.revision] as const)
		);
		if (!initialInboxLoaded) {
			knownRevisions = nextRevisions;
			initialInboxLoaded = true;
			return;
		}
		if (browserPermission === 'granted') {
			for (const item of nextInbox.items) {
				if (item.seenAtMs !== null || knownRevisions.get(item.logicalId) === item.revision)
					continue;
				const notification = new Notification(item.title, {
					body: item.body,
					tag: item.logicalId
				});
				notification.onclick = () => {
					window.focus();
					void controlClient.markNotificationSeen(item.logicalId);
					window.location.assign(deepLinkHref(item.deepLink));
				};
			}
		}
		knownRevisions = nextRevisions;
	}

	async function enableBrowserNotifications(): Promise<void> {
		if (!('Notification' in window)) return;
		browserPermission = await Notification.requestPermission();
	}

	function openNewRule(): void {
		const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
		editorRecord = null;
		editorDraft = createNotificationRule(`rule-${crypto.randomUUID()}`, timezone);
		conflict = null;
		status = null;
	}

	function openRule(record: NotificationRuleRecord): void {
		editorRecord = record;
		editorDraft = editableDraft(record.draft);
		conflict = null;
		status = null;
	}

	function closeEditor(): void {
		if (editorBusy) return;
		editorDraft = null;
		editorRecord = null;
		conflict = null;
	}

	async function saveDraft(): Promise<NotificationRuleRecord | null> {
		if (!editorDraft || editorBusy) return null;
		editorBusy = true;
		conflict = null;
		try {
			const saved = await controlClient.saveNotificationRuleDraft(
				editorDraft,
				editorRecord?.draftRevision ?? 0n
			);
			editorRecord = saved;
			editorDraft = editableDraft(saved.draft);
			status = 'Draft saved.';
			await reload(false);
			return saved;
		} catch (cause) {
			handleEditorError(cause);
			return null;
		} finally {
			editorBusy = false;
		}
	}

	async function saveAndActivate(): Promise<void> {
		if (!editorDraft || editorBusy) return;
		const saved = await saveDraft();
		if (!saved) return;
		editorBusy = true;
		try {
			const activated = await controlClient.activateNotificationRule(
				saved.id,
				saved.activeRevision,
				saved.draftRevision
			);
			editorRecord = activated;
			editorDraft = editableDraft(activated.draft);
			status = 'Rule activated.';
			await reload(false);
		} catch (cause) {
			handleEditorError(cause);
		} finally {
			editorBusy = false;
		}
	}

	function handleEditorError(cause: unknown): void {
		if (cause instanceof NotificationConflictError) {
			conflict = `Server revisions changed to active ${cause.activeRevision} and draft ${cause.draftRevision}. Your draft remains open.`;
			return;
		}
		error = message(cause, 'Notification rule update failed.');
	}

	async function toggleRule(record: NotificationRuleRecord): Promise<void> {
		const draft = structuredClone(record.draft);
		draft.enabled = !(record.active?.enabled ?? draft.enabled);
		try {
			const saved = await controlClient.saveNotificationRuleDraft(draft, record.draftRevision);
			await controlClient.activateNotificationRule(
				saved.id,
				saved.activeRevision,
				saved.draftRevision
			);
			status = draft.enabled ? 'Rule enabled.' : 'Rule disabled.';
			await reload(false);
		} catch (cause) {
			error = message(cause, 'Unable to change rule state.');
		}
	}

	async function duplicateRule(record: NotificationRuleRecord): Promise<void> {
		const draft = structuredClone(record.draft);
		draft.id = `rule-${crypto.randomUUID()}`;
		draft.name = `${draft.name} copy`;
		draft.owner_id = '';
		draft.revision = 0;
		for (const action of draft.actions) {
			action.destination = '';
			action.destination_configured = false;
			action.destination_ref = undefined;
		}
		try {
			const saved = await controlClient.saveNotificationRuleDraft(draft, 0n);
			await reload(false);
			openRule(saved);
			status = 'Rule duplicated as a draft.';
		} catch (cause) {
			error = message(cause, 'Unable to duplicate rule.');
		}
	}

	async function deleteRule(record: NotificationRuleRecord): Promise<void> {
		if (pendingDeleteId !== record.id) {
			pendingDeleteId = record.id;
			return;
		}
		try {
			await controlClient.deleteNotificationRule(
				record.id,
				record.activeRevision,
				record.draftRevision
			);
			pendingDeleteId = null;
			status = 'Rule deleted.';
			await reload(false);
		} catch (cause) {
			error = message(cause, 'Unable to delete rule.');
		}
	}

	async function testRule(record: NotificationRuleRecord): Promise<void> {
		try {
			const result = await controlClient.testNotificationRule(record.id);
			status = `Test queued ${result.queuedAttempts} channel attempt${result.queuedAttempts === 1 ? '' : 's'}.`;
			await reload(false);
		} catch (cause) {
			error = message(cause, 'Notification test failed.');
		}
	}

	async function markSeen(item: NotificationItem): Promise<void> {
		await mutateReceipt(() => controlClient.markNotificationSeen(item.logicalId));
	}

	async function acknowledge(item: NotificationItem): Promise<void> {
		await mutateReceipt(() => controlClient.acknowledgeNotification(item.logicalId));
	}

	async function clearItem(item: NotificationItem): Promise<void> {
		await mutateReceipt(() => controlClient.clearNotification(item.logicalId));
	}

	async function clearAll(): Promise<void> {
		if (!pendingClearAll) {
			pendingClearAll = true;
			return;
		}
		try {
			const cleared = await controlClient.clearNotifications({ kind: 'all' });
			pendingClearAll = false;
			status = `Cleared ${cleared} notification${cleared === 1n ? '' : 's'}.`;
			await reload(false);
		} catch (cause) {
			error = message(cause, 'Unable to clear notifications.');
		}
	}

	async function mutateReceipt(operation: () => Promise<void>): Promise<void> {
		try {
			await operation();
			await reload(false);
		} catch (cause) {
			error = message(cause, 'Unable to update notification state.');
		}
	}

	async function openItem(event: MouseEvent, item: NotificationItem): Promise<void> {
		event.preventDefault();
		try {
			await controlClient.markNotificationSeen(item.logicalId);
		} finally {
			window.location.assign(deepLinkHref(item.deepLink));
		}
	}

	function toggleTrigger(trigger: NotificationTrigger, checked: boolean): void {
		if (!editorDraft) return;
		editorDraft.triggers = checked
			? [...new Set([...editorDraft.triggers, trigger])]
			: editorDraft.triggers.filter((candidate) => candidate !== trigger);
	}

	function editableDraft(rule: NotificationRuleDefinition): NotificationRuleDefinition {
		const draft = structuredClone(rule);
		for (const action of draft.actions) {
			action.enabled ??= true;
			if (action.channel === 'push') action.pushover ??= createPushoverConfig();
		}
		if (draft.cooldowns.length === 0) {
			draft.cooldowns.push({ scope: 'camera_event_kind', duration_ms: 30_000 });
		}
		if (draft.rate_limits.length === 0) {
			draft.rate_limits.push({ scope: 'rule', maximum: 20, window_ms: 60_000 });
		}
		if (draft.schedule.quiet_hours && draft.schedule.quiet_hours.windows.length === 0) {
			draft.schedule.quiet_hours.windows.push({
				weekdays: [...allWeekdays],
				start_minute: 22 * 60,
				end_minute: 7 * 60
			});
		}
		return draft;
	}

	function updateCsv(field: 'source_ids' | 'event_kinds' | 'zones', value: string): void {
		if (!editorDraft) return;
		editorDraft.filter[field] = [
			...new Set(
				value
					.split(',')
					.map((part) => part.trim())
					.filter(Boolean)
			)
		];
	}

	function toggleQuietHours(enabled: boolean): void {
		if (!editorDraft) return;
		editorDraft.schedule.quiet_hours = enabled
			? {
					windows: [
						{
							weekdays: [...allWeekdays],
							start_minute: 22 * 60,
							end_minute: 7 * 60
						}
					]
				}
			: null;
	}

	function updateQuietTime(edge: 'start_minute' | 'end_minute', value: string): void {
		const window = editorDraft?.schedule.quiet_hours?.windows[0];
		if (!window) return;
		const [hours, minutes] = value.split(':').map(Number);
		if (!Number.isInteger(hours) || !Number.isInteger(minutes)) return;
		window[edge] = hours * 60 + minutes;
	}

	function addAction(): void {
		if (!editorDraft || editorDraft.actions.length >= 8) return;
		editorDraft.actions.push({
			enabled: true,
			channel: 'browser',
			destination: '',
			template: {
				title: '{{event.kind}} at {{source.id}}',
				body: 'Open {{notification.deep_link}}'
			},
			attachment: 'when_available',
			allow_second_delivery: false
		});
	}

	function removeAction(index: number): void {
		if (!editorDraft || editorDraft.actions.length === 1) return;
		editorDraft.actions.splice(index, 1);
	}

	function updateActionChannel(
		action: NotificationRuleDefinition['actions'][number],
		channel: NotificationChannel
	): void {
		action.channel = channel;
		action.destination = '';
		action.destination_configured = false;
		action.destination_ref = undefined;
		action.pushover = channel === 'push' ? createPushoverConfig() : undefined;
	}

	function pushoverConfig(action: NotificationAction): PushoverPublicConfig {
		return action.pushover ?? createPushoverConfig();
	}

	function pushoverSecret(action: NotificationAction, field: PushoverSecret): string {
		if (action.destination === '') return '';
		try {
			const destination = JSON.parse(action.destination) as Partial<PushoverDestination>;
			return typeof destination[field] === 'string' ? destination[field] : '';
		} catch {
			return '';
		}
	}

	function updatePushoverSecret(
		action: NotificationAction,
		field: PushoverSecret,
		value: string
	): void {
		const destination: PushoverDestination = {
			application_token: pushoverSecret(action, 'application_token'),
			user_key: pushoverSecret(action, 'user_key'),
			...pushoverConfig(action),
			[field]: value
		};
		action.destination = JSON.stringify(destination);
		action.destination_configured = false;
		action.destination_ref = undefined;
	}

	function updatePushoverConfig(
		action: NotificationAction,
		field: keyof PushoverPublicConfig,
		value: string | number | null
	): void {
		action.pushover = { ...pushoverConfig(action), [field]: value } as PushoverPublicConfig;
		if (field === 'priority') {
			if (value === 2) {
				action.pushover.retry_seconds ??= 30;
				action.pushover.expire_seconds ??= 300;
			} else {
				action.pushover.retry_seconds = null;
				action.pushover.expire_seconds = null;
			}
		}
		if (action.destination !== '') {
			action.destination = JSON.stringify({
				application_token: pushoverSecret(action, 'application_token'),
				user_key: pushoverSecret(action, 'user_key'),
				...action.pushover
			});
		}
	}

	function updateActionDestination(
		action: NotificationRuleDefinition['actions'][number],
		destination: string
	): void {
		action.destination = destination;
		if (destination !== '') {
			action.destination_configured = false;
			action.destination_ref = undefined;
		}
	}

	function moveAction(index: number, direction: -1 | 1): void {
		if (!editorDraft) return;
		const target = index + direction;
		if (target < 0 || target >= editorDraft.actions.length) return;
		const [action] = editorDraft.actions.splice(index, 1);
		editorDraft.actions.splice(target, 0, action!);
	}

	function actionDestinationLabel(channel: NotificationChannel): string {
		if (channel === 'webhook') return 'Webhook URL';
		if (channel === 'forwarder') return 'Forwarder target';
		return 'Current principal';
	}

	function ruleSummary(record: NotificationRuleRecord): string {
		const rule = record.active ?? record.draft;
		return rule.triggers.map(triggerLabel).join(', ');
	}

	function triggerLabel(trigger: NotificationTrigger): string {
		return triggerOptions.find((option) => option.value === trigger)?.label ?? trigger;
	}

	function channelSummary(record: NotificationRuleRecord): string {
		return (record.active ?? record.draft).actions.map((action) => action.channel).join(', ');
	}

	function scheduleSummary(record: NotificationRuleRecord): string {
		const schedule = (record.active ?? record.draft).schedule;
		return schedule.quiet_hours
			? `Quiet hours · ${schedule.timezone}`
			: `Always · ${schedule.timezone}`;
	}

	function cooldownSummary(record: NotificationRuleRecord): string {
		const cooldown = (record.active ?? record.draft).cooldowns[0];
		return cooldown ? `${Math.round(cooldown.duration_ms / 1_000)}s · ${cooldown.scope}` : 'None';
	}

	function channelHealth(channel: NotificationChannel): string {
		const attempt = history
			.flatMap((group) => group.attempts)
			.filter((candidate) => candidate.channel === channel)
			.toSorted((left, right) => right.attemptedAtMs - left.attemptedAtMs)[0];
		if (!attempt) return channel === 'browser' ? 'Ready' : 'Not tested';
		if (attempt.providerAcknowledgementState === 'pending') return 'Awaiting acknowledgement';
		if (
			attempt.providerAcknowledgementState === 'expired' ||
			attempt.providerAcknowledgementState === 'failed'
		)
			return 'Attention';
		if (attempt.outcome === 'delivered') return 'Healthy';
		if (attempt.outcome === 'retried') return 'Retrying';
		return 'Attention';
	}

	function nextEligible(group: NotificationHistoryGroup): number | null {
		return (
			group.events
				.map((event) => event.nextEligibleAtMs)
				.filter((value): value is number => value !== null)
				.toSorted((left, right) => right - left)[0] ?? null
		);
	}

	function formatMinute(minute: number): string {
		return `${Math.floor(minute / 60)
			.toString()
			.padStart(2, '0')}:${(minute % 60).toString().padStart(2, '0')}`;
	}

	function formatTime(value: number | null): string {
		return value === null
			? 'Never'
			: new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(
					value
				);
	}

	function deepLinkHref(value: string): string {
		const parsed = new URL(value, window.location.origin);
		const base =
			parsed.pathname === '/events'
				? resolve('/events')
				: parsed.pathname === '/system-health'
					? resolve('/system-health')
					: resolve('/');
		return `${base}${parsed.search}${parsed.hash}`;
	}

	function message(cause: unknown, fallback: string): string {
		return cause instanceof Error ? cause.message : fallback;
	}
</script>

<section
	id="notifications"
	class="scroll-mt-4 overflow-hidden rounded-md border border-hairline bg-surface"
	aria-labelledby="notifications-heading"
>
	<header class="flex flex-wrap items-end justify-between gap-4 border-b border-hairline px-5 py-5">
		<div>
			<p class="font-mono text-2xs tracking-caps text-primary-soft">NOTIFICATIONS · LIVE</p>
			<h2 id="notifications-heading" class="mt-1 text-xl font-semibold">Notification rules</h2>
			<div class="mt-2 flex flex-wrap gap-x-4 gap-y-1 font-mono text-2xs text-text-muted">
				<span>{activeRuleCount}/{ruleCount} ACTIVE</span>
				<span>{inbox.unreadCount} UNREAD</span>
				<span>{suppressionCount} SUPPRESSED OR COLLAPSED</span>
			</div>
		</div>
		<div class="flex items-center gap-2">
			{#if browserPermission !== 'granted' && browserPermission !== 'unsupported'}
				<Button variant="outline" size="sm" onclick={enableBrowserNotifications}>
					<BellIcon /> Enable browser alerts
				</Button>
			{/if}
			<Button
				variant="outline"
				size="icon-sm"
				onclick={() => reload(false)}
				disabled={refreshing}
				aria-label="Refresh notifications"
				title="Refresh notifications"
			>
				<RefreshCwIcon class={refreshing ? 'animate-spin' : ''} />
			</Button>
			<Button size="sm" onclick={openNewRule}><PlusIcon /> Add rule</Button>
		</div>
	</header>

	<div
		class="flex items-center justify-between gap-3 border-b border-hairline px-5"
		role="tablist"
		aria-label="Notification views"
	>
		<div class="flex">
			{#each [{ id: 'rules' as const, label: 'Rules', icon: BellIcon }, { id: 'inbox' as const, label: 'Inbox', icon: InboxIcon }, { id: 'history' as const, label: 'History', icon: HistoryIcon }] as tab (tab.id)}
				<button
					type="button"
					role="tab"
					aria-selected={view === tab.id}
					class="flex h-11 items-center gap-2 border-b-2 px-3 text-sm font-medium {view === tab.id
						? 'border-primary text-text'
						: 'border-transparent text-text-muted hover:text-text'}"
					onclick={() => (view = tab.id)}
				>
					<tab.icon class="size-4" />
					{tab.label}
					{#if tab.id === 'inbox' && inbox.unreadCount > 0n}
						<span
							class="min-w-5 rounded-full bg-primary px-1.5 text-center font-mono text-2xs text-primary-foreground"
							>{inbox.unreadCount}</span
						>
					{/if}
				</button>
			{/each}
		</div>
		{#if view === 'inbox' && inbox.items.length > 0}
			<Button variant={pendingClearAll ? 'destructive' : 'ghost'} size="sm" onclick={clearAll}>
				<TrashIcon />
				{pendingClearAll ? 'Confirm clear all' : 'Clear all'}
			</Button>
		{/if}
	</div>

	{#if error}
		<div
			class="flex items-start gap-2 border-b border-destructive/40 bg-destructive/5 px-5 py-3 text-sm text-destructive"
			role="alert"
		>
			<CircleAlertIcon class="mt-0.5 size-4 shrink-0" />
			{error}
		</div>
	{/if}
	{#if status}
		<div
			class="flex items-center gap-2 border-b border-primary/30 bg-primary/5 px-5 py-2.5 text-sm"
			role="status"
		>
			<CheckIcon class="size-4 text-primary" />
			{status}
		</div>
	{/if}

	{#if loading}
		<div class="grid min-h-48 place-items-center font-mono text-xs text-text-faint">
			LOADING NOTIFICATIONS
		</div>
	{:else if view === 'rules'}
		<div class="overflow-x-auto">
			<table class="w-full min-w-[960px] border-collapse text-left text-xs">
				<thead
					class="border-b border-hairline bg-raised/50 font-mono text-2xs tracking-caps text-text-faint"
				>
					<tr>
						<th class="px-5 py-3 font-medium">Rule</th>
						<th class="px-3 py-3 font-medium">Triggers</th>
						<th class="px-3 py-3 font-medium">Channels</th>
						<th class="px-3 py-3 font-medium">Schedule</th>
						<th class="px-3 py-3 font-medium">Cooldown</th>
						<th class="px-3 py-3 font-medium">Last match / delivery</th>
						<th class="px-5 py-3 text-right font-medium">Actions</th>
					</tr>
				</thead>
				<tbody class="divide-y divide-hairline">
					{#each rules as record (record.id)}
						<tr class="align-top hover:bg-raised/30">
							<td class="px-5 py-4">
								<div class="flex items-center gap-2">
									<span
										class="size-2 rounded-full {record.active?.enabled
											? 'bg-primary'
											: 'bg-text-faint'}"
									></span>
									<span class="font-semibold">{(record.active ?? record.draft).name}</span>
								</div>
								<p class="mt-1 font-mono text-2xs text-text-faint">
									r{record.activeRevision} active · r{record.draftRevision} draft
								</p>
							</td>
							<td class="max-w-56 px-3 py-4 text-text-muted">{ruleSummary(record)}</td>
							<td class="px-3 py-4 text-text-muted capitalize">{channelSummary(record)}</td>
							<td class="px-3 py-4 text-text-muted">{scheduleSummary(record)}</td>
							<td class="px-3 py-4 text-text-muted">{cooldownSummary(record)}</td>
							<td class="px-3 py-4 font-mono text-2xs text-text-muted">
								<div>{formatTime(record.lastMatchAtMs)}</div>
								<div class="mt-1">{formatTime(record.lastDeliveryAtMs)}</div>
							</td>
							<td class="px-5 py-3">
								<div class="flex justify-end gap-1">
									<Button
										variant="ghost"
										size="icon-sm"
										onclick={() => openRule(record)}
										aria-label={`Edit ${record.draft.name}`}
										title="Edit rule"><PencilIcon /></Button
									>
									<Button
										variant="ghost"
										size="icon-sm"
										onclick={() => testRule(record)}
										aria-label={`Test ${record.draft.name}`}
										title="Test rule"><FlaskConicalIcon /></Button
									>
									<Button
										variant="ghost"
										size="icon-sm"
										onclick={() => toggleRule(record)}
										aria-label={`${record.active?.enabled ? 'Disable' : 'Enable'} ${record.draft.name}`}
										title={record.active?.enabled ? 'Disable rule' : 'Enable rule'}
										><BellIcon /></Button
									>
									<Button
										variant="ghost"
										size="icon-sm"
										onclick={() => duplicateRule(record)}
										aria-label={`Duplicate ${record.draft.name}`}
										title="Duplicate rule"><CopyIcon /></Button
									>
									<Button
										variant={pendingDeleteId === record.id ? 'destructive' : 'ghost'}
										size="icon-sm"
										onclick={() => deleteRule(record)}
										aria-label={`${pendingDeleteId === record.id ? 'Confirm delete' : 'Delete'} ${record.draft.name}`}
										title={pendingDeleteId === record.id ? 'Confirm delete' : 'Delete rule'}
										><TrashIcon /></Button
									>
								</div>
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
		{#if rules.length === 0}
			<div class="grid min-h-52 place-items-center border-t border-hairline px-5 text-center">
				<div>
					<BellIcon class="mx-auto size-5 text-text-faint" />
					<p class="mt-2 text-sm font-medium">No notification rules</p>
				</div>
			</div>
		{/if}

		<div class="grid border-t border-hairline md:grid-cols-4">
			{#each ['browser', 'push', 'webhook', 'forwarder'] as channel (channel)}
				<div class="border-b border-hairline px-5 py-3 last:border-r-0 md:border-r md:border-b-0">
					<p class="font-mono text-2xs tracking-caps text-text-faint uppercase">{channel}</p>
					<p class="mt-1 text-sm font-medium">{channelHealth(channel as NotificationChannel)}</p>
				</div>
			{/each}
		</div>
	{:else if view === 'inbox'}
		<div class="divide-y divide-hairline">
			{#each inbox.items as item (item.logicalId)}
				<article
					class="grid gap-3 px-5 py-4 sm:grid-cols-[1fr_auto] {item.seenAtMs === null
						? 'bg-primary/5'
						: ''}"
				>
					<div class="min-w-0">
						<div class="flex flex-wrap items-center gap-2">
							{#if item.seenAtMs === null}<span
									class="size-2 rounded-full bg-primary"
									aria-label="Unread"
								></span>{/if}
							<h3 class="truncate text-sm font-semibold">{item.title}</h3>
							<span class="font-mono text-2xs text-text-faint uppercase">{item.stage}</span>
							<span class="font-mono text-2xs text-text-faint uppercase">{item.severity}</span>
						</div>
						<p class="mt-1 text-sm text-text-muted">{item.body}</p>
						<p class="mt-2 font-mono text-2xs text-text-faint">
							{item.sourceId} · {formatTime(item.updatedAtMs)}
						</p>
					</div>
					<div class="flex items-center gap-1">
						<a
							href={deepLinkHref(item.deepLink)}
							onclick={(event) => openItem(event, item)}
							class="inline-flex size-8 items-center justify-center rounded-md hover:bg-raised"
							aria-label={`Open ${item.title}`}
							title="Open notification"><ExternalLinkIcon class="size-4" /></a
						>
						{#if item.seenAtMs === null}<Button
								variant="ghost"
								size="icon-sm"
								onclick={() => markSeen(item)}
								aria-label={`Mark ${item.title} seen`}
								title="Mark seen"><EyeIcon /></Button
							>{/if}
						{#if item.acknowledgedAtMs === null}<Button
								variant="ghost"
								size="icon-sm"
								onclick={() => acknowledge(item)}
								aria-label={`Acknowledge ${item.title}`}
								title="Acknowledge"><CheckCheckIcon /></Button
							>{/if}
						<Button
							variant="ghost"
							size="icon-sm"
							onclick={() => clearItem(item)}
							aria-label={`Clear ${item.title}`}
							title="Clear"><XIcon /></Button
						>
					</div>
				</article>
			{/each}
		</div>
		{#if inbox.items.length === 0}<div
				class="grid min-h-52 place-items-center text-sm text-text-muted"
			>
				Inbox clear
			</div>{/if}
	{:else}
		<div class="divide-y divide-hairline">
			{#each history as group (group.notification.logicalId)}
				<details class="group px-5 py-4">
					<summary
						class="flex cursor-pointer list-none flex-wrap items-center justify-between gap-3"
					>
						<div>
							<h3 class="text-sm font-semibold">{group.notification.title}</h3>
							<p class="mt-1 font-mono text-2xs text-text-faint">
								{group.notification.stage} · {group.events.length} decisions · {group.attempts
									.length} attempts
							</p>
						</div>
						{#if nextEligible(group)}<span class="font-mono text-2xs text-text-muted"
								>NEXT ELIGIBLE {formatTime(nextEligible(group))}</span
							>{/if}
					</summary>
					<div class="mt-4 grid gap-5 border-t border-hairline pt-4 lg:grid-cols-2">
						<div>
							<h4 class="font-mono text-2xs tracking-caps text-text-faint">DECISIONS</h4>
							<ul class="mt-2 space-y-2">
								{#each group.events as event (event.sequence)}
									<li class="flex justify-between gap-4 text-xs">
										<span
											><strong class="font-medium capitalize">{event.outcome}</strong
											>{#if event.reason}<span class="text-text-muted">
													· {event.reason}</span
												>{/if}</span
										><time class="shrink-0 font-mono text-2xs text-text-faint"
											>{formatTime(event.occurredAtMs)}</time
										>
									</li>
								{/each}
							</ul>
						</div>
						<div>
							<h4 class="font-mono text-2xs tracking-caps text-text-faint">CHANNEL ATTEMPTS</h4>
							<ul class="mt-2 space-y-2">
								{#each group.attempts as attempt (attempt.sequence)}
									<li class="flex justify-between gap-4 text-xs">
										<span
											><strong class="font-medium capitalize"
												>{attempt.channel} · {attempt.outcome}</strong
											><span class="font-mono text-2xs text-text-faint">
												· {attempt.targetHash.slice(0, 10)}</span
											>{#if attempt.providerRequestId}<span
													class="font-mono text-2xs text-text-faint"
												>
													· request {attempt.providerRequestId.slice(0, 12)}</span
												>{/if}{#if attempt.providerAcknowledgementState}<span
													class="text-text-muted"
												>
													· {attempt.providerAcknowledgementState}</span
												>{/if}{#if attempt.reason}<span class="text-text-muted">
													· {attempt.reason}</span
												>{/if}</span
										><time class="shrink-0 font-mono text-2xs text-text-faint"
											>{formatTime(attempt.attemptedAtMs)}</time
										>
									</li>
								{/each}
							</ul>
						</div>
					</div>
				</details>
			{/each}
		</div>
		{#if history.length === 0}<div class="grid min-h-52 place-items-center text-sm text-text-muted">
				No delivery history
			</div>{/if}
	{/if}

	{#if editorDraft}
		<div
			class="border-t-2 border-primary bg-ground/40"
			role="dialog"
			aria-modal="false"
			aria-labelledby="notification-editor-title"
		>
			<header class="flex items-center justify-between border-b border-hairline px-5 py-4">
				<div>
					<p class="font-mono text-2xs tracking-caps text-primary-soft">RULE EDITOR</p>
					<h3 id="notification-editor-title" class="mt-1 text-lg font-semibold">
						{editorRecord ? editorDraft.name : 'New notification rule'}
					</h3>
				</div>
				<Button
					variant="ghost"
					size="icon-sm"
					onclick={closeEditor}
					aria-label="Close rule editor"
					title="Close"><XIcon /></Button
				>
			</header>
			{#if conflict}<div
					class="flex gap-2 border-b border-activity/40 bg-activity/5 px-5 py-3 text-sm text-activity"
					role="alert"
				>
					<CircleAlertIcon class="mt-0.5 size-4 shrink-0" />
					{conflict}
				</div>{/if}
			<form
				class="grid gap-0 lg:grid-cols-[minmax(0,1fr)_320px]"
				onsubmit={(event) => {
					event.preventDefault();
					void saveAndActivate();
				}}
			>
				<div class="divide-y divide-hairline">
					<fieldset class="grid gap-4 p-5 sm:grid-cols-2">
						<legend class="sr-only">Identity</legend>
						<label class="space-y-1.5 text-xs font-medium"
							>Rule name<Input bind:value={editorDraft.name} required maxlength={128} /></label
						>
						<label class="space-y-1.5 text-xs font-medium"
							>Rule ID<Input
								bind:value={editorDraft.id}
								required
								maxlength={128}
								disabled={editorRecord !== null}
							/></label
						>
						<label class="flex items-center gap-2 text-xs font-medium"
							><input
								type="checkbox"
								bind:checked={editorDraft.enabled}
								class="size-4 accent-primary"
							/> Enabled when activated</label
						>
					</fieldset>

					<fieldset class="p-5">
						<legend class="text-sm font-semibold">Triggers and scope</legend>
						<div class="mt-3 flex flex-wrap gap-x-5 gap-y-2">
							{#each triggerOptions as option (option.value)}<label
									class="flex items-center gap-2 text-xs"
									><input
										type="checkbox"
										checked={editorDraft.triggers.includes(option.value)}
										onchange={(event) => toggleTrigger(option.value, event.currentTarget.checked)}
										class="size-4 accent-primary"
									/>
									{option.label}</label
								>{/each}
						</div>
						<div class="mt-4 grid gap-4 sm:grid-cols-3">
							<label class="space-y-1.5 text-xs font-medium"
								>Camera/source IDs<Input
									value={editorDraft.filter.source_ids.join(', ')}
									oninput={(event) => updateCsv('source_ids', event.currentTarget.value)}
									placeholder="All sources"
								/></label
							>
							<label class="space-y-1.5 text-xs font-medium"
								>Event kinds<Input
									value={editorDraft.filter.event_kinds.join(', ')}
									oninput={(event) => updateCsv('event_kinds', event.currentTarget.value)}
									placeholder="person, vehicle"
								/></label
							>
							<label class="space-y-1.5 text-xs font-medium"
								>Zones<Input
									value={editorDraft.filter.zones.join(', ')}
									oninput={(event) => updateCsv('zones', event.currentTarget.value)}
									placeholder="All zones"
								/></label
							>
							<label class="space-y-1.5 text-xs font-medium"
								>Minimum confidence<Input
									type="number"
									min="0"
									max="1"
									step="0.01"
									value={editorDraft.filter.minimum_confidence ?? ''}
									oninput={(event) =>
										(editorDraft!.filter.minimum_confidence =
											event.currentTarget.value === '' ? null : Number(event.currentTarget.value))}
								/></label
							>
							<label class="space-y-1.5 text-xs font-medium"
								>Minimum duration (seconds)<Input
									type="number"
									min="0"
									value={(editorDraft.filter.minimum_duration_ms ?? 0) / 1000}
									oninput={(event) =>
										(editorDraft!.filter.minimum_duration_ms =
											Number(event.currentTarget.value) > 0
												? Number(event.currentTarget.value) * 1000
												: null)}
								/></label
							>
							<label class="space-y-1.5 text-xs font-medium"
								>Image predicate<select
									bind:value={editorDraft.filter.attachment_required}
									class="h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
									><option value={null}>Any</option><option value={true}>Image required</option
									><option value={false}>No image</option></select
								></label
							>
						</div>
					</fieldset>

					<fieldset class="grid gap-4 p-5 sm:grid-cols-3">
						<legend class="text-sm font-semibold">Schedule and suppression</legend>
						<label class="space-y-1.5 text-xs font-medium"
							>Timezone<Input bind:value={editorDraft.schedule.timezone} required /></label
						>
						<label class="space-y-1.5 text-xs font-medium"
							>Cooldown scope<select
								bind:value={editorDraft.cooldowns[0].scope}
								class="h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
								><option value="event">Event family</option><option value="camera_event_kind"
									>Camera + event kind</option
								><option value="group">Group</option><option value="rule">Whole rule</option><option
									value="outage">Outage interval</option
								></select
							></label
						>
						<label class="space-y-1.5 text-xs font-medium"
							>Cooldown (seconds)<Input
								type="number"
								min="1"
								max="2592000"
								value={editorDraft.cooldowns[0].duration_ms / 1000}
								oninput={(event) =>
									(editorDraft!.cooldowns[0]!.duration_ms =
										Number(event.currentTarget.value) * 1000)}
								required
							/></label
						>
						<label class="flex items-center gap-2 text-xs font-medium"
							><input
								type="checkbox"
								checked={editorDraft.schedule.quiet_hours !== null}
								onchange={(event) => toggleQuietHours(event.currentTarget.checked)}
								class="size-4 accent-primary"
							/> Quiet hours</label
						>
						{#if editorDraft.schedule.quiet_hours}<label class="space-y-1.5 text-xs font-medium"
								>Quiet starts<Input
									type="time"
									value={formatMinute(editorDraft.schedule.quiet_hours.windows[0].start_minute)}
									oninput={(event) => updateQuietTime('start_minute', event.currentTarget.value)}
								/></label
							><label class="space-y-1.5 text-xs font-medium"
								>Quiet ends<Input
									type="time"
									value={formatMinute(editorDraft.schedule.quiet_hours.windows[0].end_minute)}
									oninput={(event) => updateQuietTime('end_minute', event.currentTarget.value)}
								/></label
							>{/if}
						<label class="space-y-1.5 text-xs font-medium"
							>Rule limit / minute<Input
								type="number"
								min="1"
								max="10000"
								bind:value={editorDraft.rate_limits[0].maximum}
							/></label
						>
						<label class="flex items-center gap-2 text-xs font-medium"
							><input
								type="checkbox"
								checked={editorDraft.critical_bypass !== null}
								onchange={(event) =>
									(editorDraft!.critical_bypass = event.currentTarget.checked
										? { maximum: 2, window_ms: 60000 }
										: null)}
								class="size-4 accent-primary"
							/> Bounded critical bypass</label
						>
					</fieldset>

					<fieldset class="p-5">
						<div class="flex items-center justify-between">
							<legend class="text-sm font-semibold">Ordered actions</legend><Button
								variant="outline"
								size="sm"
								onclick={addAction}
								disabled={editorDraft.actions.length >= 8}><PlusIcon /> Add action</Button
							>
						</div>
						<div class="mt-3 divide-y divide-hairline border-y border-hairline">
							{#each editorDraft.actions as action, index (index)}
								<div class="grid gap-3 py-4 sm:grid-cols-[120px_minmax(0,1fr)_auto]">
									<div class="space-y-3">
										<label class="flex items-center gap-2 text-xs font-medium"
											><input
												type="checkbox"
												bind:checked={action.enabled}
												class="size-4 accent-primary"
											/> Enabled</label
										>
										<label class="space-y-1.5 text-xs font-medium"
											>Channel<select
												value={action.channel}
												onchange={(event) =>
													updateActionChannel(
														action,
														event.currentTarget.value as NotificationChannel
													)}
												class="h-9 w-full rounded-md border border-input bg-background px-2 text-sm"
												><option value="browser">Browser</option><option value="push">Push</option
												><option value="webhook">Webhook</option><option value="forwarder"
													>Forwarder</option
												></select
											></label
										>
									</div>
									<div class="grid gap-3 sm:grid-cols-2">
										{#if action.channel === 'push'}
											<PushoverActionFields
												{action}
												secret={(field) => pushoverSecret(action, field)}
												onsecret={(field, value) => updatePushoverSecret(action, field, value)}
												onconfig={(field, value) => updatePushoverConfig(action, field, value)}
											/>
										{:else}
											<label class="space-y-1.5 text-xs font-medium"
												>{actionDestinationLabel(action.channel)}<Input
													value={action.destination}
													placeholder={action.destination_configured ? 'Configured' : ''}
													oninput={(event) =>
														updateActionDestination(action, event.currentTarget.value)}
													disabled={action.channel === 'browser'}
													required={action.channel !== 'browser' && !action.destination_configured}
												/></label
											>
										{/if}
										<label class="space-y-1.5 text-xs font-medium"
											>Attachment<select
												bind:value={action.attachment}
												class="h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
												><option value="never">Never</option><option value="when_available"
													>When available</option
												><option value="required">Required</option></select
											></label
										>
										<label class="space-y-1.5 text-xs font-medium"
											>Title template<Input
												bind:value={action.template.title}
												maxlength={256}
											/></label
										>
										<label class="space-y-1.5 text-xs font-medium"
											>Body template<Input
												bind:value={action.template.body}
												maxlength={4096}
											/></label
										>
										<label class="flex items-center gap-2 text-xs"
											><input
												type="checkbox"
												bind:checked={action.allow_second_delivery}
												class="size-4 accent-primary"
											/> Allow second delivery without replacement</label
										>
									</div>
									<div class="flex gap-1">
										<Button
											variant="ghost"
											size="icon-sm"
											onclick={() => moveAction(index, -1)}
											disabled={index === 0}
											aria-label="Move action up"
											title="Move up"><ChevronUpIcon /></Button
										><Button
											variant="ghost"
											size="icon-sm"
											onclick={() => moveAction(index, 1)}
											disabled={index === editorDraft!.actions.length - 1}
											aria-label="Move action down"
											title="Move down"><ChevronDownIcon /></Button
										><Button
											variant="ghost"
											size="icon-sm"
											onclick={() => removeAction(index)}
											disabled={editorDraft!.actions.length === 1}
											aria-label="Remove action"
											title="Remove action"><TrashIcon /></Button
										>
									</div>
								</div>
							{/each}
						</div>
					</fieldset>

					<fieldset class="grid gap-4 p-5 sm:grid-cols-4">
						<legend class="text-sm font-semibold">Delivery bounds</legend>
						<label class="space-y-1.5 text-xs font-medium"
							>Enrichment deadline (seconds)<Input
								type="number"
								min="1"
								value={editorDraft.enrichment.deadline_ms / 1000}
								oninput={(event) =>
									(editorDraft!.enrichment.deadline_ms = Number(event.currentTarget.value) * 1000)}
							/></label
						>
						<label class="space-y-1.5 text-xs font-medium"
							>Maximum revisions<Input
								type="number"
								min="1"
								max="32"
								bind:value={editorDraft.enrichment.maximum_revisions}
							/></label
						>
						<label class="space-y-1.5 text-xs font-medium"
							>Delivery attempts<Input
								type="number"
								min="1"
								max="10"
								bind:value={editorDraft.failure.maximum_attempts}
							/></label
						>
						<label class="space-y-1.5 text-xs font-medium"
							>Outbox expiry (minutes)<Input
								type="number"
								min="1"
								value={editorDraft.failure.expiry_ms / 60000}
								oninput={(event) =>
									(editorDraft!.failure.expiry_ms = Number(event.currentTarget.value) * 60000)}
							/></label
						>
					</fieldset>
				</div>

				<aside class="border-t border-hairline bg-raised/40 p-5 lg:border-t-0 lg:border-l">
					<h4 class="font-mono text-2xs tracking-caps text-text-faint">EFFECTIVE POLICY</h4>
					<dl class="mt-3 divide-y divide-hairline border-y border-hairline text-xs">
						<div class="py-3">
							<dt class="text-text-muted">Matches</dt>
							<dd class="mt-1 font-medium">
								{editorDraft.triggers.map(triggerLabel).join(', ') || 'No triggers'}
							</dd>
						</div>
						<div class="py-3">
							<dt class="text-text-muted">Sources</dt>
							<dd class="mt-1 font-medium">
								{editorDraft.filter.source_ids.join(', ') || 'All authorized sources'}
							</dd>
						</div>
						<div class="py-3">
							<dt class="text-text-muted">Channels</dt>
							<dd class="mt-1 font-medium capitalize">
								{editorDraft.actions.map((action) => action.channel).join(' → ')}
							</dd>
						</div>
						<div class="py-3">
							<dt class="text-text-muted">Suppression starts</dt>
							<dd class="mt-1 font-medium">Logical notification creation</dd>
						</div>
						<div class="py-3">
							<dt class="text-text-muted">Quiet hours</dt>
							<dd class="mt-1 font-medium">
								{editorDraft.schedule.quiet_hours
									? `${formatMinute(editorDraft.schedule.quiet_hours.windows[0].start_minute)}–${formatMinute(editorDraft.schedule.quiet_hours.windows[0].end_minute)}`
									: 'Disabled'}
							</dd>
						</div>
						<div class="py-3">
							<dt class="text-text-muted">Enrichment</dt>
							<dd class="mt-1 font-medium">
								{editorDraft.enrichment.deadline_ms / 1000}s · {editorDraft.enrichment
									.maximum_revisions} revisions
							</dd>
						</div>
					</dl>
					<div class="mt-5 flex flex-col gap-2">
						<Button
							type="submit"
							disabled={editorBusy ||
								editorDraft.triggers.length === 0 ||
								editorDraft.actions.length === 0}><CheckIcon /> Save & activate</Button
						><Button variant="outline" disabled={editorBusy} onclick={() => void saveDraft()}
							><SaveIcon /> Save draft</Button
						>
					</div>
				</aside>
			</form>
		</div>
	{/if}
</section>
