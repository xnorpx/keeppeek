<script lang="ts">
	import type { CameraRecordingMode, EventRecordingStream } from '$lib/types';
	import { Input } from '$lib/components/ui/input/index.js';

	type Props = {
		mode: CameraRecordingMode;
		preRecordingSupported?: boolean;
		duration: string;
		preDuration: string;
		stream: EventRecordingStream;
		onduration: (value: string) => void;
		onpre: (value: string) => void;
		onstream: (value: EventRecordingStream) => void;
	};
	let {
		mode,
		preRecordingSupported = false,
		duration,
		preDuration,
		stream,
		onduration,
		onpre,
		onstream
	}: Props = $props();
	const helpId = $props.id();
	const controlClass =
		'h-10 w-full rounded-sm border border-hairline bg-raised px-3 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring';
</script>

{#if mode === 'event-boost' || mode === 'event-only'}
	<div class="grid gap-4 sm:grid-cols-2" data-event-recording-fields>
		<label class="grid gap-1.5 text-sm font-medium">
			{mode === 'event-boost'
				? 'Main recording after an event (seconds)'
				: 'Recording after an event (seconds)'}
			<Input
				value={duration}
				inputmode="numeric"
				autocomplete="off"
				oninput={(event) => onduration(String(event.currentTarget.value))}
			/>
		</label>
		{#if preRecordingSupported}
			<label class="grid gap-1.5 text-sm font-medium">
				Pre-recording duration (seconds)
				<Input
					value={preDuration}
					inputmode="numeric"
					autocomplete="off"
					aria-describedby={helpId}
					oninput={(event) => onpre(String(event.currentTarget.value))}
				/>
			</label>
			{#if mode === 'event-only'}
				<label class="grid gap-1.5 text-sm font-medium">
					Event recording stream
					<select
						class={controlClass}
						value={stream}
						onchange={(event) => onstream(event.currentTarget.value as EventRecordingStream)}
					>
						<option value="main">Main</option>
						<option value="sub">Sub</option>
					</select>
				</label>
			{/if}
			<p id={helpId} class="text-xs leading-5 text-text-muted sm:col-span-2">
				Choose 0 to 30 seconds before an event; 0 disables pre-recording. Available history depends
				on keyframes and memory limits.
				{#if mode === 'event-boost'}
					Pre-recording delays saving continuous video by up to this duration. A crash can lose that
					unsaved video.
				{:else}
					Video is saved only during event windows. Idle history stays in memory.
				{/if}
			</p>
		{/if}
	</div>
{/if}
