<script lang="ts">
	import type { PrivacySchedule, PrivacySchedulePatch, PrivacyWindow } from '$lib/types';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import PlusIcon from '@lucide/svelte/icons/plus';
	import SaveIcon from '@lucide/svelte/icons/save';
	import Trash2Icon from '@lucide/svelte/icons/trash-2';
	import XIcon from '@lucide/svelte/icons/x';

	type Props = {
		schedule: PrivacySchedule | null;
		statusSource?: 'none' | 'default' | 'camera';
		saving?: boolean;
		error?: string | null;
		oncancel?: () => void;
		onsave: (patch: PrivacySchedulePatch) => void | Promise<void>;
	};

	let {
		schedule,
		statusSource = 'none',
		saving = false,
		error = null,
		oncancel,
		onsave
	}: Props = $props();
	let form = $state<PrivacySchedule>(copySchedule(null));
	let loadedSchedule = $state<PrivacySchedule | null>(null);
	let validationError = $state<string | null>(null);
	let overrideEnabled = $state(false);
	let overrideReason = $state('');
	let overrideDurationMinutes = $state('60');

	$effect(() => {
		if (schedule !== loadedSchedule) {
			loadedSchedule = schedule;
			form = copySchedule(schedule);
			overrideEnabled = schedule?.temporary_override !== null;
			overrideReason = schedule?.temporary_override?.reason ?? '';
			overrideDurationMinutes = schedule?.temporary_override
				? String(
						Math.max(
							1,
							Math.ceil(
								(schedule.temporary_override.expires_at_ms -
									schedule.temporary_override.accepted_at_ms) /
									60_000
							)
						)
					)
				: '60';
		}
	});

	const days = [
		['M', 1],
		['T', 2],
		['W', 3],
		['T', 4],
		['F', 5],
		['S', 6],
		['S', 7]
	] as const;
	function copySchedule(value: PrivacySchedule | null): PrivacySchedule {
		return value
			? {
					enabled: value.enabled,
					timezone: value.timezone,
					windows: value.windows.map((window) => ({ ...window, weekdays: [...window.weekdays] })),
					temporary_override: value.temporary_override ? { ...value.temporary_override } : null,
					keep_camera_connected: value.keep_camera_connected
				}
			: {
					enabled: true,
					timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
					windows: [],
					temporary_override: null,
					keep_camera_connected: true
				};
	}

	function addWindow(): void {
		form.windows.push({ weekdays: [1, 2, 3, 4, 5], start: '22:00', end: '07:00' });
	}

	function removeWindow(index: number): void {
		form.windows.splice(index, 1);
	}

	function toggleDay(window: PrivacyWindow, day: number): void {
		window.weekdays = window.weekdays.includes(day)
			? window.weekdays.filter((value) => value !== day)
			: [...window.weekdays, day].sort((left, right) => left - right);
	}

	function submit(event: SubmitEvent): void {
		event.preventDefault();
		if (saving) return;
		validationError = null;
		if (!form.timezone.trim()) {
			validationError = 'Timezone is required.';
			return;
		}
		if (form.windows.some((window) => window.weekdays.length === 0)) {
			validationError = 'Every privacy window needs at least one weekday.';
			return;
		}
		const duration = Number(overrideDurationMinutes);
		if (
			overrideEnabled &&
			(!overrideReason.trim() || !Number.isInteger(duration) || duration < 1 || duration > 1440)
		) {
			validationError = 'Temporary overrides need a reason and a duration from 1 to 1440 minutes.';
			return;
		}
		const value = copySchedule(form);
		value.temporary_override = overrideEnabled
			? {
					actor: '',
					reason: overrideReason.trim(),
					accepted_at_ms: Date.now(),
					expires_at_ms: Date.now() + duration * 60_000
				}
			: null;
		void onsave({ operation: 'set', value });
	}
</script>

<form
	class="scroll-mt-16 overflow-hidden rounded-md border border-hairline bg-surface"
	onsubmit={submit}
	aria-labelledby="privacy-editor-heading"
>
	<fieldset disabled={saving} class="contents">
		<header
			class="flex flex-wrap items-start justify-between gap-4 border-b border-hairline px-4 py-4"
		>
			<div>
				<p class="font-mono text-2xs tracking-caps text-primary-soft">SERVER PRIVACY POLICY</p>
				<h2 id="privacy-editor-heading" class="mt-1 text-lg font-semibold">
					Edit privacy schedule
				</h2>
				<p class="mt-1 text-xs leading-5 text-text-muted">
					Administrators configure recurring windows in an IANA timezone. Enforcement remains
					server-side.
				</p>
			</div>
			{#if oncancel}
				<Button type="button" variant="ghost" size="sm" onclick={oncancel}>
					<XIcon /> Close
				</Button>
			{/if}
		</header>

		<div class="grid gap-5 p-4">
			<div class="grid gap-4 sm:grid-cols-2">
				<label class="flex items-center gap-2 text-sm font-medium" for="privacy-enabled">
					<input
						id="privacy-enabled"
						type="checkbox"
						bind:checked={form.enabled}
						class="size-4 accent-primary"
					/>
					Schedule enabled
				</label>
				<label class="grid gap-1.5 text-sm font-medium" for="privacy-timezone">
					IANA timezone
					<Input
						id="privacy-timezone"
						bind:value={form.timezone}
						placeholder="America/Los_Angeles"
					/>
				</label>
			</div>

			<div class="grid gap-3 rounded-sm border border-hairline bg-raised p-3">
				<label class="flex items-center gap-2 text-sm font-medium" for="privacy-keep-connected">
					<input
						id="privacy-keep-connected"
						type="checkbox"
						bind:checked={form.keep_camera_connected}
						class="size-4 accent-primary"
					/>
					Keep camera connection warm during privacy
				</label>
				<p class="text-xs text-text-muted">
					Media delivery, recording, snapshots, and controls remain blocked. Keeping ingress warm
					shortens recovery when the schedule ends.
				</p>
			</div>

			<section
				class="grid gap-3 rounded-sm border border-hairline bg-raised p-3"
				aria-labelledby="privacy-override-heading"
			>
				<div>
					<h3 id="privacy-override-heading" class="text-sm font-semibold">Temporary override</h3>
					<p class="mt-1 text-xs text-text-muted">
						Administrator-only. The server records the actor and acceptance time.
					</p>
				</div>
				<label class="flex items-center gap-2 text-sm font-medium" for="privacy-override-enabled">
					<input
						id="privacy-override-enabled"
						type="checkbox"
						bind:checked={overrideEnabled}
						class="size-4 accent-primary"
					/>
					Allow privacy only until the override expires
				</label>
				{#if overrideEnabled}
					<div class="grid gap-3 sm:grid-cols-[minmax(0,1fr)_10rem]">
						<label class="grid gap-1.5 text-sm font-medium" for="privacy-override-reason">
							Reason
							<Input
								id="privacy-override-reason"
								bind:value={overrideReason}
								maxlength={256}
								placeholder="Maintenance"
							/>
						</label>
						<label class="grid gap-1.5 text-sm font-medium" for="privacy-override-duration">
							Duration (minutes)
							<Input
								id="privacy-override-duration"
								type="number"
								min="1"
								max="1440"
								step="1"
								bind:value={overrideDurationMinutes}
							/>
						</label>
					</div>
				{/if}
			</section>

			<div class="space-y-3" aria-labelledby="privacy-windows-heading">
				<div class="flex flex-wrap items-center justify-between gap-3">
					<div>
						<h3 id="privacy-windows-heading" class="text-sm font-semibold">Recurring windows</h3>
						<p class="mt-1 text-xs text-text-muted">
							End times are exclusive. Overnight windows may cross midnight.
						</p>
					</div>
					<Button type="button" variant="outline" size="sm" onclick={addWindow}>
						<PlusIcon /> Add window
					</Button>
				</div>
				{#if form.windows.length === 0}
					<p
						class="rounded-sm border border-dashed border-hairline-strong p-3 text-xs text-text-muted"
					>
						No recurring windows. The policy is configured but never active until a window is added.
					</p>
				{:else}
					{#each form.windows as window, index (index)}
						<div
							class="grid gap-3 rounded-sm border border-hairline bg-raised p-3 lg:grid-cols-[1fr_9rem_9rem_auto] lg:items-end"
						>
							<fieldset class="grid gap-1.5">
								<legend class="text-xs font-medium">Days</legend>
								<div class="flex flex-wrap gap-1" aria-label={`Window ${index + 1} weekdays`}>
									{#each days as [label, day]}
										<button
											type="button"
											class="grid size-8 place-items-center rounded-sm border text-xs font-semibold focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none {window.weekdays.includes(
												day
											)
												? 'border-primary bg-primary/10 text-primary'
												: 'border-hairline-strong'}"
											aria-pressed={window.weekdays.includes(day)}
											aria-label={`${label} day ${day}`}
											onclick={() => toggleDay(window, day)}
										>
											{label}
										</button>
									{/each}
								</div>
							</fieldset>
							<label class="grid gap-1.5 text-sm font-medium" for={`privacy-start-${index}`}>
								Start
								<Input id={`privacy-start-${index}`} type="time" bind:value={window.start} />
							</label>
							<label class="grid gap-1.5 text-sm font-medium" for={`privacy-end-${index}`}>
								End
								<Input id={`privacy-end-${index}`} type="time" bind:value={window.end} />
							</label>
							<Button
								type="button"
								variant="ghost"
								size="icon"
								aria-label={`Remove window ${index + 1}`}
								onclick={() => removeWindow(index)}
							>
								<Trash2Icon />
							</Button>
						</div>
					{/each}
				{/if}
			</div>
			{#if statusSource === 'default'}
				<p class="text-xs text-text-muted">
					This camera currently inherits the shared default. Saving here creates a camera-specific
					policy.
				</p>
			{/if}
		</div>
	</fieldset>

	{#if validationError || error}
		<p class="mx-4 text-sm text-destructive" role="alert">{validationError ?? error}</p>
	{/if}
	<footer class="mt-4 flex flex-wrap justify-end gap-2 border-t border-hairline px-4 py-4">
		{#if statusSource === 'camera' && oncancel}
			<Button
				type="button"
				variant="outline"
				onclick={() => void onsave({ operation: 'clear' })}
				disabled={saving}>Use shared default</Button
			>
		{/if}
		<Button type="submit" disabled={saving}>
			<SaveIcon />
			{saving ? 'Saving privacy schedule' : 'Save privacy schedule'}
		</Button>
	</footer>
</form>
