<script lang="ts">
	import type { RecordedAudioTiming } from '$lib/proto/webrtc_pb';
	import { formatRecordedAudioOffset } from '$lib/recorded-audio';
	let { timing, snapshot = false }: { timing?: RecordedAudioTiming; snapshot?: boolean } = $props();
</script>

<div class="grid min-w-0 gap-2 text-xs text-text-muted" data-recorded-audio-evidence>
	{#if timing}
		<dl class="grid min-w-0 gap-2">
			<div class="flex justify-between gap-3">
				<dt>{snapshot ? 'Recorded audio snapshot' : 'Saved configured'}</dt>
				<dd class="font-mono text-foreground">
					{formatRecordedAudioOffset(snapshot ? timing.appliedOffsetMs : timing.configuredOffsetMs)}
				</dd>
			</div>
			{#if !snapshot}<div class="flex justify-between gap-3">
					<dt>Currently applied</dt>
					<dd class="text-right font-mono text-foreground">
						{formatRecordedAudioOffset(timing.appliedOffsetMs)} / {timing.preview
							? 'preview'
							: 'configured'}
					</dd>
				</div>{/if}
			<div class="flex justify-between gap-3">
				<dt>Timing settings ID</dt>
				<dd class="max-w-[60%] min-w-0 text-right font-mono break-all">
					{timing.configurationRevision || 'Unavailable'}
				</dd>
			</div>
		</dl>
		{#if timing.audioPresent === false}<p>
				No audio in the inspected {snapshot ? 'export range' : 'source fragment'}.
			</p>
		{:else if timing.audioPresent === true}<p>
				Audio present in the inspected {snapshot ? 'export range' : 'source fragment'}.
			</p>
		{:else}<p>Audio presence has not been inspected.</p>{/if}
		{#if timing.clippedAudioSamples !== undefined}
			<p>
				{timing.clippedAudioSamples} leading encoded audio samples clipped. Advancing audio can remove
				sound at the start.
			</p>
		{/if}
		{#if snapshot}<p>
				Later setting or preview changes do not change this file's captured correction.
			</p>
		{:else}<details class="min-w-0">
				<summary class="min-h-11 cursor-pointer py-3 font-medium text-foreground"
					>Normalized container timing</summary
				>
				<dl class="grid gap-2 font-mono">
					<div class="flex justify-between gap-3">
						<dt>First video PTS</dt>
						<dd>
							{timing.firstVideoPtsUs === undefined
								? 'Unavailable'
								: `${timing.firstVideoPtsUs} µs`}
						</dd>
					</div>
					<div class="flex justify-between gap-3">
						<dt>First audio PTS</dt>
						<dd>
							{timing.firstAudioPtsUs === undefined
								? 'Unavailable'
								: `${timing.firstAudioPtsUs} µs`}
						</dd>
					</div>
					<div class="flex justify-between gap-3">
						<dt>Stored start skew</dt>
						<dd>
							{timing.storedSkewUs === undefined ? 'Unavailable' : `${timing.storedSkewUs} µs`}
						</dd>
					</div>
					<div class="flex justify-between gap-3">
						<dt>Timing mapping</dt>
						<dd>{timing.mappingVersion || 'Unavailable'}</dd>
					</div>
				</dl>
				<p class="mt-3">
					These are stored media timestamps after common normalization, not audible lip-sync.
					Positive skew means audio starts later. Raw capture-clock skew and browser buffering are
					unavailable.
				</p>
			</details>{/if}
	{:else}
		<p>Recorded audio timing unavailable.</p>
		<p>No correction snapshot was reported. Do not assume zero correction.</p>
	{/if}
</div>
