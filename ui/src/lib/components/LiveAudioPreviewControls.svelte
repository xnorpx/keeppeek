<script lang="ts">
	import { tick } from 'svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import {
		formatRecordedAudioOffset,
		parseRecordedAudioOffset,
		timingEditorKinds
	} from '$lib/recorded-audio';

	type Props = {
		helpId: string;
		onpreview: (stream: 'main' | 'sub', offset: number | null) => Promise<void>;
		appliedPreview?: number;
		previewRequested: boolean;
	};
	let { helpId, onpreview, appliedPreview, previewRequested }: Props = $props();
	let error = $state<string | null>(null);
	let status = $state<string | null>(null);
	let errorSummary = $state<HTMLParagraphElement>();
	let previewStream = $state<'main' | 'sub'>('main');
	let previewInput = $state('0');
	let previewing = $state(false);
	let parsedPreview = $derived.by(() => {
		try {
			return parseRecordedAudioOffset(previewInput, timingEditorKinds.live.limit);
		} catch {
			return null;
		}
	});
	async function preview(offset: number | null) {
		if (previewing) return;
		previewing = true;
		status = null;
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
</script>

<div class="space-y-2">
	<h3 class="text-sm font-semibold">Preview live timing</h3>
	<p class="text-xs text-text-muted">Preview applies to the selected stream in this viewer.</p>
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
				class="h-11"
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
			<p class="text-warning text-xs">Video will play {Math.abs(parsedPreview)} ms later.</p>
		{/if}</div
	>
	<div class="flex flex-wrap gap-2">
		<Button
			class="h-11"
			type="button"
			variant="outline"
			disabled={previewing || parsedPreview === null}
			onclick={() => preview(parsedPreview)}>Apply preview</Button
		>
		<Button
			class="h-11"
			type="button"
			variant="outline"
			disabled={previewing || (appliedPreview === undefined && !previewRequested)}
			onclick={() => preview(null)}>Reset preview</Button
		>
	</div>
	{#if appliedPreview !== undefined}<p class="text-xs text-text-muted"
			>Active preview {formatRecordedAudioOffset(appliedPreview)}</p
		>{/if}
</div>
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
