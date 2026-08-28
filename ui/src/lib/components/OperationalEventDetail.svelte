<script lang="ts">
	import type { RecordingEvent } from '$lib/types';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';
	import XIcon from '@lucide/svelte/icons/x';

	type Props = {
		event: RecordingEvent;
		onclose?: () => void;
	};

	let { event, onclose }: Props = $props();
	let clockNowMs = $state(Date.now());
	let evidence = $derived(event.operational);
	let durationMs = $derived(
		event.end_time_ms === null
			? Math.max(0, clockNowMs - event.start_time_ms)
			: (evidence?.duration_ms ?? Math.max(0, event.end_time_ms - event.start_time_ms))
	);
	const timestampFormatter = new Intl.DateTimeFormat(undefined, {
		month: 'short',
		day: 'numeric',
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit',
		timeZone: 'UTC',
		timeZoneName: 'short'
	});

	function label(value: string): string {
		const normalized = value.replaceAll(/[-_]/g, ' ').trim();
		return normalized ? normalized.charAt(0).toUpperCase() + normalized.slice(1) : 'Unknown';
	}

	function formatDuration(value: number): string {
		const totalSeconds = Math.max(0, Math.round(value / 1_000));
		const hours = Math.floor(totalSeconds / 3_600);
		const minutes = Math.floor((totalSeconds % 3_600) / 60);
		const seconds = totalSeconds % 60;
		return [hours && `${hours}h`, (hours || minutes) && `${minutes}m`, `${seconds}s`]
			.filter(Boolean)
			.join(' ');
	}

	$effect(() => {
		if (event.end_time_ms !== null) return;
		const timer = window.setInterval(() => (clockNowMs = Date.now()), 1_000);
		return () => window.clearInterval(timer);
	});
</script>

{#if evidence}
	<section
		data-operational-event-detail={event.id}
		class="border-y border-hairline bg-surface px-4 py-3"
		aria-label="Operational event details"
	>
		<header class="flex items-start gap-3">
			<TriangleAlertIcon
				class="mt-0.5 size-4 shrink-0 {evidence.severity === 'critical'
					? 'text-live'
					: 'text-activity'}"
			/>
			<div class="min-w-0 flex-1">
				<div class="flex flex-wrap items-center gap-2">
					<h2 class="text-sm font-semibold">{label(evidence.kind)}</h2>
					<span
						class="rounded-sm px-1.5 py-0.5 font-mono text-[10px] font-semibold tracking-caps {evidence.severity ===
						'critical'
							? 'bg-live/15 text-live'
							: 'bg-activity/15 text-activity'}"
					>
						{evidence.recovered ? 'RECOVERED' : 'ONGOING'}
					</span>
				</div>
				<p class="mt-1 text-xs text-text-muted">{evidence.explanation}</p>
				<p class="mt-1 font-mono text-[10px] text-text-faint">CAUSE · {evidence.cause}</p>
			</div>
			<button
				type="button"
				class="grid size-8 shrink-0 place-items-center rounded-sm text-text-muted hover:bg-raised hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				aria-label="Close operational event details"
				onclick={() => onclose?.()}
			>
				<XIcon class="size-4" />
			</button>
		</header>

		<dl class="mt-3 grid grid-cols-2 gap-x-4 gap-y-2 text-xs md:grid-cols-3">
			<div>
				<dt class="font-mono text-[10px] tracking-caps text-text-faint">START</dt>
				<dd>{timestampFormatter.format(new Date(event.start_time_ms))}</dd>
			</div>
			<div>
				<dt class="font-mono text-[10px] tracking-caps text-text-faint">RECOVERY</dt>
				<dd>
					{event.end_time_ms === null
						? 'Not recovered'
						: timestampFormatter.format(new Date(event.end_time_ms))}
				</dd>
			</div>
			<div>
				<dt class="font-mono text-[10px] tracking-caps text-text-faint">DURATION</dt>
				<dd>{formatDuration(durationMs)}</dd>
			</div>
			<div>
				<dt class="font-mono text-[10px] tracking-caps text-text-faint">AFFECTED STREAMS</dt>
				<dd>{evidence.affected_streams.join(', ') || 'Camera'}</dd>
			</div>
			<div>
				<dt class="font-mono text-[10px] tracking-caps text-text-faint">RECORDING</dt>
				<dd>{evidence.recording_interrupted ? 'Interrupted' : 'Not interrupted'}</dd>
			</div>
			<div>
				<dt class="font-mono text-[10px] tracking-caps text-text-faint">EVIDENCE SOURCE</dt>
				<dd>{label(evidence.evidence_source)}</dd>
			</div>
		</dl>
	</section>
{/if}
