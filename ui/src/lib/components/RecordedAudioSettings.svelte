<script lang="ts">
	import { tick, untrack } from 'svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import {
		effectiveRecordedAudioOffset,
		formatRecordedAudioOffset,
		parseRecordedAudioOffset
	} from '$lib/recorded-audio';
	import type { CameraSettings, RecordedAudioSettings } from '$lib/types';

	type Props = {
		camera: CameraSettings;
		kind?: 'recorded' | 'live';
		onpreview?: (stream: 'main' | 'sub', offset: number | null) => Promise<void>;
		appliedPreview?: number;
		disabled?: boolean;
		onsave: (settings: RecordedAudioSettings, revision: string) => Promise<CameraSettings>;
	};
	type Key = keyof RecordedAudioSettings;
	type Draft = Record<Key, { mode: 'inherit' | 'custom'; text: string }>;
	let {
		camera,
		disabled = false,
		onsave,
		kind = 'recorded',
		onpreview,
		appliedPreview
	}: Props = $props();
	const live = $derived(kind === 'live');
	let previewStream = $state<'main' | 'sub'>('main');
	let previewInput = $state('0');
	let previewing = $state(false);
	let parsedPreview = $derived.by(() => {
		try {
			return parseRecordedAudioOffset(previewInput, 200);
		} catch {
			return null;
		}
	});
	async function preview(offset: number | null) {
		if (!onpreview || previewing) return;
		previewing = true;
		error = null;
		try {
			await onpreview(previewStream, offset);
			status = offset === null ? 'Saved live timing restored.' : 'Preview applied to this viewer.';
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Live preview failed.';
			await tick();
			errorSummary?.focus();
		} finally {
			previewing = false;
		}
	}
	const fields = [
		{ key: 'default_offset_ms', label: 'Camera default' },
		{ key: 'main_offset_ms', label: 'Main stream' },
		{ key: 'sub_offset_ms', label: 'Sub stream' }
	] as const;
	const helpId = $props.id();
	let saved = $state.raw(untrack(() => snapshot(camera)));
	let draft = $state<Draft>(untrack(() => draftFrom(saved.settings)));
	let saving = $state(false);
	let error = $state<string | null>(null);
	let status = $state<string | null>(null);
	let errorSummary = $state<HTMLParagraphElement>();
	let dirty = $derived(JSON.stringify(draft) !== JSON.stringify(draftFrom(saved.settings)));
	let parsed = $derived.by(() => {
		try {
			const settings: RecordedAudioSettings = {};
			for (const { key } of fields) {
				if (draft[key].mode === 'custom')
					settings[key] = parseRecordedAudioOffset(draft[key].text, live ? 200 : 5000);
			}
			return { settings, error: null };
		} catch (cause) {
			return {
				settings: null,
				error: cause instanceof Error ? cause.message : 'Invalid audio offset.'
			};
		}
	});
	let largeOffset = $derived(
		parsed.settings !== null &&
			Object.values(parsed.settings).some((value) => Math.abs(value) > 1000)
	);
	let changedElsewhere = $derived(camera.configuration_revision !== saved.revision);

	function snapshot(value: CameraSettings) {
		return {
			settings: { ...(kind === 'live' ? value.live_audio : value.recorded_audio) },
			revision: value.configuration_revision ?? ''
		};
	}

	function draftFrom(settings: RecordedAudioSettings): Draft {
		return {
			default_offset_ms: choice(settings.default_offset_ms),
			main_offset_ms: choice(settings.main_offset_ms),
			sub_offset_ms: choice(settings.sub_offset_ms)
		};
	}

	function choice(value: number | undefined): Draft[Key] {
		return { mode: value === undefined ? 'inherit' : 'custom', text: String(value ?? 0) };
	}

	function effective(settings: RecordedAudioSettings, key: Key): number {
		if (key === 'default_offset_ms') return settings.default_offset_ms ?? 0;
		return effectiveRecordedAudioOffset(settings, key === 'main_offset_ms' ? 'main' : 'sub');
	}

	function reload(): void {
		saved = snapshot(camera);
		draft = draftFrom(saved.settings);
		error = null;
		status = null;
	}

	async function submit(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (
			saving ||
			disabled ||
			!dirty ||
			(live ? camera.live_audio : camera.recorded_audio) === undefined
		)
			return;
		error = null;
		status = null;
		try {
			if (parsed.error || !parsed.settings)
				throw new Error(parsed.error ?? 'Invalid audio offset.');
			if (!saved.revision) throw new Error('Reload camera settings before saving timing.');
			saving = true;
			const result = await onsave(parsed.settings, saved.revision);
			if (result.id !== camera.id) throw new Error('The camera changed while saving timing.');
			saved = snapshot(result);
			draft = draftFrom(saved.settings);
			status = live ? 'Live timing saved.' : 'Recorded timing saved. Camera recording continues.';
		} catch (cause) {
			error = cause instanceof Error ? cause.message : 'Timing was not saved.';
			await tick();
			errorSummary?.focus();
		} finally {
			saving = false;
		}
	}
</script>

<section
	class="min-w-0 rounded-md border border-hairline bg-surface"
	aria-labelledby={`${helpId}-heading`}
	data-recorded-audio-settings={live ? undefined : true}
	data-live-audio-settings={live ? true : undefined}
>
	<header class="border-b border-hairline p-4">
		<p class="font-mono text-2xs tracking-caps text-primary-soft"
			>{live ? 'LIVE AUDIO' : 'RECORDED AUDIO'}</p
		>
		<h2 id={`${helpId}-heading`} class="mt-1 text-base font-semibold"
			>{live ? 'Live' : 'Recorded'} audio synchronization</h2
		>
		<p id={helpId} class="mt-1 text-xs leading-5 text-text-muted">
			{#if live}
				Positive offsets delay audio. Negative offsets delay video. Use whole milliseconds from −200
				to +200. Explicit 0 disables correction at that level.
			{:else}
				Positive offsets delay audio. Negative offsets advance it. Source recordings stay unchanged.
				Use whole milliseconds from −5000 to +5000. Explicit 0 disables correction at that level.
			{/if}
		</p>
	</header>
	{#if (live ? camera.live_audio : camera.recorded_audio) === undefined}
		<p class="p-4 text-sm text-text-muted">
			{live ? 'Live-audio' : 'Recorded-audio'} settings are unavailable on this server.
		</p>
	{:else}
		<form class="min-w-0" onsubmit={submit}>
			<fieldset disabled={saving || disabled} class="min-w-0 space-y-4 p-4">
				{#each fields as field (field.key)}
					<div class="min-w-0 rounded-sm border border-hairline bg-raised p-3">
						<p class="text-sm font-semibold">{field.label}</p>
						<p class="mt-1 text-xs text-text-muted">
							Saved: {saved.settings[field.key] === undefined
								? 'Inherited'
								: formatRecordedAudioOffset(saved.settings[field.key]!)}
							· Effective: {formatRecordedAudioOffset(effective(saved.settings, field.key))}
						</p>
						<div class="mt-3 grid min-w-0 gap-2 sm:grid-cols-2">
							<label class="grid min-w-0 gap-1.5 text-xs font-medium">
								Mode
								<select
									aria-label={`${field.label} mode`}
									bind:value={draft[field.key].mode}
									class="h-11 w-full min-w-0 rounded-sm border border-hairline bg-background px-3 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring"
								>
									<option value="inherit"
										>{field.key === 'default_offset_ms'
											? 'Inherit · 0 ms'
											: 'Inherit camera default'}</option
									>
									<option value="custom">Custom offset</option>
								</select>
							</label>
							<label class="grid min-w-0 gap-1.5 text-xs font-medium">
								Offset (ms)
								<Input
									aria-label={`${field.label} offset (ms)`}
									class="h-11 w-full min-w-0 font-mono"
									inputmode="numeric"
									autocomplete="off"
									aria-describedby={live ? `${helpId} ${helpId}-saved-latency` : helpId}
									disabled={draft[field.key].mode === 'inherit'}
									bind:value={draft[field.key].text}
								/>
							</label>
						</div>
						<p class="mt-2 text-xs text-text-muted">
							Draft effective: {parsed.settings
								? formatRecordedAudioOffset(effective(parsed.settings, field.key))
								: 'Invalid offset'}
						</p>
					</div>
				{/each}
				{#if live}<div id={`${helpId}-saved-latency`} role="status">
						{#if parsed.settings}
							{#each ['main', 'sub'] as stream}
								{@const offset = effectiveRecordedAudioOffset(
									parsed.settings,
									stream === 'main' ? 'main' : 'sub'
								)}
								{#if offset < 0}
									<p class="text-warning text-xs leading-5"
										>{stream === 'main' ? 'Main' : 'Sub'} video will play {Math.abs(offset)} ms later
										after saving.</p
									>
								{/if}
							{/each}
						{/if}</div
					>{/if}
				{#if live && onpreview}
					<div class="space-y-2">
						<h3 class="text-sm font-semibold">Preview live timing</h3>
						<p class="text-xs text-text-muted"
							>Preview applies to the selected stream in this viewer.</p
						>
						<div class="grid gap-2 sm:grid-cols-2">
							<label class="grid gap-1 text-xs"
								>Preview stream
								<select
									class="h-11 rounded-sm border border-hairline bg-background px-3"
									bind:value={previewStream}
								>
									<option value="main">Main stream</option><option value="sub">Sub stream</option>
								</select></label
							>
							<label class="grid gap-1 text-xs"
								>Preview offset (ms)<Input
									inputmode="numeric"
									bind:value={previewInput}
									aria-describedby={`${helpId} ${helpId}-preview-notice`}
									aria-invalid={parsedPreview === null}
									onkeydown={(event) => {
										if (event.key === 'Enter') {
											event.preventDefault();
											if (parsedPreview !== null) void preview(parsedPreview);
										}
									}}
								/></label
							>
						</div>
						<div id={`${helpId}-preview-notice`} role="status"
							>{#if parsedPreview === null}
								<p class="text-xs text-destructive">Enter a whole number from −200 to +200 ms.</p>
							{:else if parsedPreview < 0}
								<p class="text-warning text-xs"
									>Video will play {Math.abs(parsedPreview)} ms later.</p
								>
							{/if}</div
						>
						<div class="flex flex-wrap gap-2">
							<Button
								type="button"
								variant="outline"
								disabled={previewing || parsedPreview === null}
								onclick={() => preview(parsedPreview)}>Apply preview</Button
							>
							<Button
								type="button"
								variant="outline"
								disabled={previewing || appliedPreview === undefined}
								onclick={() => preview(null)}>Reset preview</Button
							>
						</div>
						{#if appliedPreview !== undefined}<p class="text-xs text-text-muted"
								>Active preview {formatRecordedAudioOffset(appliedPreview)}</p
							>{/if}
					</div>
				{/if}
				{#if largeOffset}
					<p class="text-warning text-xs leading-5" role="status">
						Large offset: confirm synchronization against a visible sound before saving.
					</p>
				{/if}
				{#if changedElsewhere}
					<p class="text-xs leading-5 text-text-muted">
						Saved configuration changed. Your draft keeps its original revision until you reload
						saved values.
					</p>
				{/if}
				{#if error}
					<p
						bind:this={errorSummary}
						tabindex="-1"
						class="text-sm text-destructive outline-none"
						role="alert"
					>
						{error}
					</p>
				{/if}
				{#if status}<p class="text-xs text-healthy" role="status">{status}</p>{/if}
				<div class="flex flex-col gap-2 sm:flex-row sm:flex-wrap">
					<Button type="submit" class="h-11 w-full sm:w-auto" disabled={!dirty}
						>{saving ? 'Saving…' : live ? 'Save live timing' : 'Save recorded timing'}</Button
					>
					<Button type="button" variant="outline" class="h-11 w-full sm:w-auto" onclick={reload}
						>Reload saved values</Button
					>
				</div>
			</fieldset>
		</form>
	{/if}
	{#if !live}
		<footer class="border-t border-hairline p-4 text-xs leading-5 text-text-muted">
			<p>
				Applies to recorded playback and new exports. Existing export retries retain their original
				timing.
			</p>
			{#if camera.live_audio === undefined}<p>Live audio correction is unavailable.</p>{/if}
		</footer>
	{/if}
</section>
