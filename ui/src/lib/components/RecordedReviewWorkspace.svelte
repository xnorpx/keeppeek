<script lang="ts">
	import { onDestroy, onMount, tick, untrack } from 'svelte';
	import { resolve } from '$app/paths';
	import CopyMomentLink from './CopyMomentLink.svelte';
	import { MAX_EXPORT_DURATION_MS, segmentForExportRange } from '$lib/keep-modes';
	import { recordedPlaybackRates } from './RecordedPlaybackControls.svelte';
	import { GridPlaybackSession, maximumReviewCameras } from '$lib/grid-playback-session';
	import { RecordedReviewRequests, reviewExportRecordings } from '$lib/recorded-review-requests';
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
	import type { CameraListItem } from '$lib/types';
	import type { StoredTimelineRange } from '$lib/control-client';
	import type { RecordedStreamId } from '$lib/recorded-playback-policy';
	import RecordedReviewTile from './RecordedReviewTile.svelte';

	let {
		cameras,
		selection,
		startMs,
		focusedId,
		preferences,
		updateLink,
		openExport,
		single
	}: {
		cameras: CameraListItem[];
		selection: KeepReviewSelection;
		startMs: number;
		focusedId: string;
		preferences: PlaybackPreferenceDocument;
		updateLink: (at: number, focus: string, selection: KeepReviewSelection) => void;
		openExport: (
			at: number,
			focus: string,
			stream: RecordedStreamId,
			selection: KeepReviewSelection
		) => void;
		single: (at: number, focus: string) => void;
	} = $props();
	const workspaceId = $props.id();
	let optionsOpen = $state(false);
	let optionsButton = $state<HTMLButtonElement>();
	let focusControl = $state<HTMLSelectElement>();
	const client = useControlClient();
	const requests = new RecordedReviewRequests();
	const clock = new GridPlaybackSession();
	let focus = $state(untrack(() => focusedId));
	let audio = $state(untrack(() => selection.audioCameraId));
	let muted = $state(untrack(() => preferences.media.muted));
	let epoch = $state(untrack(() => startMs));
	let playing = $state(false);
	let rangeStart = $state(untrack(() => selection.range?.startMs ?? null));
	let rangeEnd = $state(untrack(() => selection.range?.endMs ?? null));
	const range = $derived(
		rangeStart !== null &&
			rangeEnd !== null &&
			rangeEnd > rangeStart &&
			rangeEnd - rangeStart <= MAX_EXPORT_DURATION_MS
			? { startMs: rangeStart, endMs: rangeEnd }
			: undefined
	);
	let ids = $state(untrack(() => [...selection.cameraIds]));
	const reviewSelection = $derived({
		cameraIds: ids,
		audioCameraId: audio,
		...(range ? { range } : {})
	});
	let revision = $state(0);
	let budget = $state(2);
	let visible = $state<Record<string, number>>({});
	let foreground = $state(false);
	let rate = $state(untrack(() => preferences.media.playbackRate));
	let ranges = $state.raw<StoredTimelineRange[]>([]);
	let loadedWindow = $state<number | null>(null);
	let unavailable = $state(false);
	let retry = $state(0);
	let seekInput = $state(untrack(() => new Date(startMs).toISOString().slice(0, 23)));
	const authorized = $derived(cameras.filter((c) => ids.includes(c.id)));
	// Refresh halfway through each minute of coverage so a continuing camera stays admitted.
	const refreshMs = 30_000;
	const bucket = $derived(Math.floor(epoch / refreshMs) * refreshMs);
	const moment = () => Math.floor(clock.positionAt(performance.now()));
	const chosen = $derived.by(() => {
		const result = new Map<string, { stream: RecordedStreamId; endMs: number }>();
		for (const camera of authorized) {
			const covering = ranges.filter(
				(range) => range.sourceId === camera.id && range.startMs <= epoch && epoch < range.endMs
			);
			const availableStreams = covering
				.map((range) => range.streamId)
				.filter((stream) => stream === 'main' || stream === 'sub');
			const { selectedStream } = selectRecordedStream(camera, {
				preference: recordedPreference(preferences, camera.id),
				availableStreams,
				isEncodingSupported: browserSupportsRecordedEncoding
			});
			const range = covering.find((entry) => entry.streamId === selectedStream);
			if (range && selectedStream)
				result.set(camera.id, { stream: selectedStream, endMs: range.endMs });
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
		let hadSession = false;
		const unsubscribeAccess = client.onAccessState((state) => {
			const lostSession = hadSession && !state.session;
			hadSession = Boolean(state.session);
			if (!lostSession || !playing) return;
			// The sign-in gate unmounts Keep. Preserve this moment and require an explicit resume.
			clock.setPlaying(false, performance.now());
			playing = false;
			epoch = moment();
			persist();
		});
		return () => {
			unsubscribeAccess();
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
		if (!sources.length) {
			ranges = [];
			loadedWindow = from;
			return;
		}
		client
			.queryStoredTimeline({
				sourceIds: sources,
				startMs: from,
				endMs: from + 2 * refreshMs,
				includeEvents: false,
				includeAttachments: false,
				signal: controller.signal
			})
			.then((result) => {
				if (controller.signal.aborted) return;
				ranges = result.ranges;
				loadedWindow = from;
			})
			.catch(() => {
				if (controller.signal.aborted) return;
				ranges = [];
				unavailable = true;
			});
		return () => controller.abort();
	});

	let exportCheck = $state.raw<AbortController | null>(null);
	let exportProblem = $state('');
	onDestroy(() => exportCheck?.abort());
	$effect(() => {
		void focus;
		void rangeStart;
		void rangeEnd;
		exportProblem = '';
		return () => exportCheck?.abort();
	});

	async function exportMarked() {
		const camera = authorized.find((camera) => camera.id === focus),
			marked = range;
		if (!camera || !marked || exportCheck) return;
		const at = moment(),
			controller = new AbortController();
		exportCheck = controller;
		exportProblem = '';
		try {
			const rows = await reviewExportRecordings(client, camera.id, marked, controller.signal);
			if (
				controller.signal.aborted ||
				camera.id !== focus ||
				rangeStart !== marked.startMs ||
				rangeEnd !== marked.endMs
			)
				return;
			const streams = (['main', 'sub'] as const).filter((stream) =>
				segmentForExportRange(rows, stream, marked.startMs, marked.endMs)
			);
			const { selectedStream } = selectRecordedStream(camera, {
				preference: recordedPreference(preferences, camera.id),
				availableStreams: streams,
				isEncodingSupported: browserSupportsRecordedEncoding
			});
			if (selectedStream) openExport(at, camera.id, selectedStream, reviewSelection);
			else exportProblem = 'No playable stream from this camera covers the entire selected range.';
		} catch {
			if (!controller.signal.aborted)
				exportProblem = 'Recording coverage could not be loaded. Try exporting again.';
		} finally {
			if (exportCheck === controller) exportCheck = null;
		}
	}
	function markRange(edge: 'start' | 'end') {
		if (edge === 'start') rangeStart = moment();
		else rangeEnd = moment();
		persist();
	}
	function comparisonLink() {
		return recordingMomentUrl({
			origin: window.location.origin,
			keepPath: resolve('/keep'),
			eventsPath: resolve('/events'),
			cameraId: focus,
			timestampMs: moment(),
			streamPreference: recordedPreference(preferences, focus),
			mode: 'timeline',
			review: reviewSelection
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
		updateLink(moment(), focus, reviewSelection);
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
	function persistCameraSelection() {
		const previous = document.activeElement;
		persist();
		void tick().then(() => {
			if (!previous || previous.isConnected || document.activeElement !== document.body) return;
			const target = focusControl?.getClientRects().length ? focusControl : optionsButton;
			target?.focus();
		});
	}
	function addCamera(id: string) {
		if (
			ids.length >= maximumReviewCameras ||
			ids.includes(id) ||
			!cameras.some((camera) => camera.id === id)
		)
			return;
		ids = [...ids, id];
		persistCameraSelection();
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
		persistCameraSelection();
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
		<button
			type="button"
			class="min-h-11 rounded-sm border border-hairline-strong px-3 md:hidden"
			bind:this={optionsButton}
			aria-expanded={optionsOpen}
			aria-controls={workspaceId + '-settings ' + workspaceId + '-marks'}
			onclick={() => (optionsOpen = !optionsOpen)}
		>
			Comparison options
		</button>
		<div id={workspaceId + '-settings'} class={optionsOpen ? 'contents' : 'hidden md:contents'}>
			<form
				class="flex flex-wrap items-center gap-2"
				onsubmit={(event) => {
					event.preventDefault();
					seekTo(Date.parse(seekInput + 'Z'));
				}}
			>
				<label
					>Seek to UTC <input
						class="min-h-11 max-w-full"
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
						class="min-h-11 max-w-full min-w-11"
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
					class="min-h-11 max-w-full min-w-11"
					aria-label="Focused camera"
					bind:this={focusControl}
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
					class="min-h-11 max-w-full min-w-11"
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
					class="min-h-11 max-w-full min-w-11"
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
	</div>
	<div
		id={workspaceId + '-marks'}
		class="flex-wrap items-center gap-3 {optionsOpen ? 'flex' : 'hidden md:flex'}"
	>
		<button type="button" class="min-h-11 px-2" onclick={() => markRange('start')}
			>Set range start</button
		>
		{#if rangeStart !== null}<time datetime={new Date(rangeStart).toISOString()}
				>{new Date(rangeStart).toISOString()}</time
			>{/if}
		<button type="button" class="min-h-11 px-2" onclick={() => markRange('end')}
			>Set range end</button
		>
		{#if rangeEnd !== null}<time datetime={new Date(rangeEnd).toISOString()}
				>{new Date(rangeEnd).toISOString()}</time
			>{/if}
		<button
			type="button"
			class="min-h-11 rounded-sm border border-hairline px-3 disabled:opacity-45"
			disabled={!range || !authorized.some((camera) => camera.id === focus) || !!exportCheck}
			onclick={exportMarked}>Export focused camera</button
		>
		{#if exportProblem}<p role="alert">{exportProblem}</p>{/if}
		{#if rangeStart !== null && rangeEnd !== null && !range}<p role="status"
				>Choose an end after the start, within two minutes.</p
			>{/if}
	</div>
	{#if unavailable}<button class="min-h-11 px-2" type="button" onclick={() => retry++}
			>Retry coverage</button
		>{/if}
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
				endMs={segment?.endMs ?? 0}
				{epoch}
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
