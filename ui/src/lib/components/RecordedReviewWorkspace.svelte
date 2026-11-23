<script lang="ts">
	import { onMount, untrack } from 'svelte';
	import { resolve } from '$app/paths';
	import CopyMomentLink from './CopyMomentLink.svelte';
	import { recordedPlaybackRates } from './RecordedPlaybackControls.svelte';
	import { GridPlaybackSession, maximumReviewCameras } from '$lib/grid-playback-session';
	import { RecordedReviewRequests } from '$lib/recorded-review-requests';
	import { useControlClient } from '$lib/control-context';
	import {
		browserSupportsRecordedEncoding,
		selectRecordedStream
	} from '$lib/recorded-playback-policy';
	import {
		recordedPreference,
		loadPlaybackPreferences,
		savePlaybackPreferences,
		withMediaPreferences,
		type PlaybackPreferenceDocument
	} from '$lib/playback-preferences';
	import {
		parseMomentTimestamp,
		recordingMomentUrl,
		type KeepReviewSelection
	} from '$lib/keep-link';
	import type { CameraListItem, RecordingSegment, RecordingsResponse } from '$lib/types';
	import RecordedReviewTile from './RecordedReviewTile.svelte';

	let {
		cameras,
		selection,
		startMs,
		focusedId,
		preferences,
		updateLink,
		single
	}: {
		cameras: CameraListItem[];
		selection: KeepReviewSelection;
		startMs: number;
		focusedId: string;
		preferences: PlaybackPreferenceDocument;
		updateLink: (at: number, focus: string, selection: KeepReviewSelection) => void;
		single: (at: number, focus: string) => void;
	} = $props();
	const client = useControlClient();
	const requests = new RecordedReviewRequests();
	const clock = new GridPlaybackSession();
	let focus = $state(untrack(() => focusedId));
	let audio = $state(untrack(() => selection.audioCameraId));
	let muted = $state(untrack(() => preferences.media.muted));
	let epoch = $state(untrack(() => startMs));
	let playing = $state(false);
	let revision = $state(0);
	let budget = $state(2);
	let visible = $state<Record<string, number>>({});
	let foreground = $state(false);
	let rate = $state(untrack(() => preferences.media.playbackRate));
	let rows = $state.raw<RecordingsResponse[]>([]);
	let loadedWindow = $state<number | null>(null);
	let unavailable = $state(false);
	let retry = $state(0);
	let ids = $state(untrack(() => [...selection.cameraIds]));
	let seekInput = $state(untrack(() => new Date(startMs).toISOString().slice(0, 23)));
	const authorized = $derived(cameras.filter((c) => ids.includes(c.id)));
	const bucket = $derived(Math.floor(epoch / 60_000) * 60_000);
	const moment = () => Math.floor(clock.positionAt(performance.now()));
	const chosen = $derived.by(() => {
		const result = new Map<string, RecordingSegment>();
		for (const camera of authorized) {
			const segments =
				rows
					.find((row) => row.camera_id === camera.id)
					?.segments.filter(
						(segment) => segment.start_time_ms <= epoch && epoch < segment.end_time_ms
					) ?? [];
			const { selectedStream } = selectRecordedStream(camera, {
				preference: recordedPreference(preferences, camera.id),
				availableStreams: segments.map((row) => row.stream),
				isEncodingSupported: browserSupportsRecordedEncoding
			});
			const segment = segments.find((row) => row.stream === selectedStream);
			if (segment) result.set(camera.id, segment);
		}
		return result;
	});
	const active = $derived(
		clock.reconcile(
			ids.map((cameraId) => ({
				cameraId,
				visibleFraction: visible[cameraId] ?? 0,
				focused: cameraId === focus,
				hasRecording: chosen.has(cameraId)
			})),
			foreground ? budget : 0
		).activeCameraIds
	);

	onMount(() => {
		const compact = window.matchMedia('(max-width: 767px)');
		function capacity() {
			const cores = Math.max(1, Math.floor((navigator.hardwareConcurrency || 4) / 2));
			budget = Math.min(compact.matches ? 2 : maximumReviewCameras, cores);
			foreground = document.visibilityState === 'visible';
			if (foreground && playing) epoch = moment();
		}
		capacity();
		compact.addEventListener('change', capacity);
		document.addEventListener('visibilitychange', capacity);
		clock.update({
			selectedEpochMs: startMs,
			mode: 'paused',
			playbackRate: rate,
			focusedSourceId: focus,
			monotonicMs: performance.now()
		});
		return () => {
			compact.removeEventListener('change', capacity);
			document.removeEventListener('visibilitychange', capacity);
		};
	});

	$effect(() => {
		if (!playing || !foreground) return;
		const tick = window.setInterval(() => {
			epoch = moment();
		}, 125);
		return () => window.clearInterval(tick);
	});

	$effect(() => {
		if (!foreground) return;
		const from = bucket,
			sources = authorized.map((camera) => camera.id);
		void retry;
		const controller = new AbortController();
		loadedWindow = null;
		unavailable = false;
		client
			.getRecordingsInRange(
				sources,
				new Date(from).toISOString().slice(0, 10),
				from,
				from + 60_000,
				controller.signal
			)
			.then((result) => {
				if (controller.signal.aborted) return;
				rows = result;
				loadedWindow = from;
			})
			.catch(() => {
				if (controller.signal.aborted) return;
				rows = [];
				unavailable = true;
			});
		return () => controller.abort();
	});

	function comparisonLink() {
		return recordingMomentUrl({
			origin: window.location.origin,
			keepPath: resolve('/keep'),
			eventsPath: resolve('/events'),
			cameraId: focus,
			timestampMs: moment(),
			streamPreference: recordedPreference(preferences, focus),
			mode: 'timeline',
			review: { cameraIds: ids, audioCameraId: audio }
		});
	}
	function rememberMedia() {
		const latest = loadPlaybackPreferences(window.localStorage);
		savePlaybackPreferences(
			window.localStorage,
			withMediaPreferences(latest, { muted, playbackRate: rate })
		);
	}

	function persist() {
		updateLink(moment(), focus, { cameraIds: ids, audioCameraId: audio });
	}
	function seekTo(at: number) {
		const valid = parseMomentTimestamp(String(Math.floor(at)));
		if (valid === null) return;
		clock.seek(valid, performance.now());
		epoch = moment();
		seekInput = new Date(valid).toISOString().slice(0, 23);
		revision++;
		persist();
	}
	function addCamera(id: string) {
		if (
			ids.length >= maximumReviewCameras ||
			ids.includes(id) ||
			!cameras.some((camera) => camera.id === id)
		)
			return;
		ids = [...ids, id];
		persist();
	}
	function removeCamera(id: string) {
		const remaining = ids.filter((camera) => camera !== id);
		if (!remaining.length || remaining.length === ids.length) return;
		const nextFocus = remaining.includes(focus)
			? focus
			: remaining.find((candidate) => cameras.some((camera) => camera.id === candidate));
		if (!nextFocus) return;
		if (remaining.length === 1) {
			single(moment(), nextFocus);
			return;
		}
		focus = nextFocus;
		if (!remaining.includes(audio)) audio = nextFocus;
		ids = remaining;
		persist();
	}
	function togglePlaying() {
		playing = !playing;
		clock.setPlaying(playing, performance.now());
		epoch = moment();
		persist();
	}
	function label(id: string) {
		const camera = cameras.find((c) => c.id === id);
		return camera?.name || camera?.id || 'Unavailable camera';
	}
	function status(id: string) {
		if (!authorized.some((camera) => camera.id === id)) return 'Unavailable camera';
		if (unavailable) return 'Coverage unavailable';
		if (loadedWindow !== bucket) return 'Loading footage';
		if (!chosen.has(id)) return 'No playable footage at this time';
		return active.includes(id) ? 'Opening recording' : 'Waiting for a decoder';
	}
</script>

<section class="mx-auto flex h-full max-w-[120rem] flex-col gap-3 overflow-y-auto p-3 md:p-5">
	<header class="flex flex-wrap items-center justify-between gap-3">
		<h1 class="text-xl font-semibold">Compare recordings</h1>
		<CopyMomentLink getLink={comparisonLink} />
		<button type="button" class="h-11 px-3 underline" onclick={() => single(moment(), focus)}
			>Single camera</button
		>
	</header>
	<div
		class="flex flex-wrap items-center gap-3 rounded-sm border border-hairline bg-raised px-3 py-2"
	>
		<button
			type="button"
			class="h-11 px-2"
			onclick={() => seekTo(Math.max(0, moment() - 5000))}
			aria-label="Back five seconds">−5 s</button
		>
		<button
			type="button"
			class="h-11 min-w-16 rounded-sm bg-primary px-3 text-on-primary"
			aria-label="Play recordings"
			aria-pressed={playing}
			onclick={togglePlaying}>{playing ? 'Pause' : 'Play'}</button
		>
		<button
			type="button"
			class="h-11 px-2"
			onclick={() => seekTo(moment() + 5000)}
			aria-label="Forward five seconds">+5 s</button
		>
		<time class="font-mono text-sm" datetime={new Date(epoch).toISOString()}
			>{new Date(epoch).toISOString().replace('T', ' ').replace('Z', ' UTC')}</time
		>
		<form
			class="flex flex-wrap items-center gap-2"
			onsubmit={(event) => {
				event.preventDefault();
				seekTo(Date.parse(seekInput + 'Z'));
			}}
		>
			<label
				>Seek to UTC <input
					aria-label="Seek to UTC"
					type="datetime-local"
					step="0.001"
					min="1970-01-01T00:00:00"
					max="9999-12-31T23:59:59.999"
					required
					bind:value={seekInput}
				/></label
			>
			<button class="min-h-11 px-2" type="submit">Go to time</button>
		</form>
		{#if ids.length < maximumReviewCameras && cameras.some((camera) => !ids.includes(camera.id))}
			<label
				>Add camera <select
					aria-label="Add camera"
					value=""
					onchange={(event) => {
						addCamera(event.currentTarget.value);
						event.currentTarget.value = '';
					}}
				>
					<option value="">Select camera</option>
					{#each cameras.filter((camera) => !ids.includes(camera.id)) as camera (camera.id)}
						<option value={camera.id}>{label(camera.id)}</option>
					{/each}
				</select></label
			>
		{/if}
		<label
			>Focus <select
				aria-label="Focused camera"
				value={focus}
				onchange={(event) => {
					focus = event.currentTarget.value;
					persist();
				}}
				>{#each authorized as camera (camera.id)}<option value={camera.id}
						>{label(camera.id)}</option
					>{/each}</select
			></label
		>
		<label
			>Audio <select
				aria-label="Audio source"
				value={audio}
				onchange={(event) => {
					audio = event.currentTarget.value;
					persist();
				}}
				>{#each authorized as camera (camera.id)}<option value={camera.id}
						>{label(camera.id)}</option
					>{/each}</select
			></label
		>
		<button
			type="button"
			aria-label="Mute audio"
			aria-pressed={muted}
			class="h-11 px-2"
			onclick={() => {
				muted = !muted;
				rememberMedia();
			}}>{muted ? 'Unmute' : 'Mute'}</button
		>
		<label
			>Speed <select
				aria-label="Playback speed"
				value={rate}
				onchange={(event) => {
					rate = Number(event.currentTarget.value);
					clock.setRate(rate, performance.now());
					rememberMedia();
				}}
				>{#each recordedPlaybackRates as speed (speed)}<option value={speed}>{speed}×</option
					>{/each}</select
			></label
		>
	</div>
	{#if unavailable}<button type="button" onclick={() => retry++}>Retry coverage</button>{/if}
	<div class="grid grid-cols-1 gap-3 md:grid-cols-2">
		{#each ids as cameraId (cameraId)}
			{@const segment = chosen.get(cameraId)}
			<RecordedReviewTile
				onremove={() => removeCamera(cameraId)}
				onvisibility={(fraction) => {
					visible[cameraId] = fraction;
				}}
				{requests}
				{cameraId}
				name={label(cameraId)}
				stream={active.includes(cameraId) ? (segment?.stream ?? null) : null}
				endMs={segment?.end_time_ms ?? 0}
				{revision}
				position={moment}
				{playing}
				{rate}
				muted={muted || cameraId !== audio}
				status={status(cameraId)}
			/>
		{/each}
	</div>
</section>
