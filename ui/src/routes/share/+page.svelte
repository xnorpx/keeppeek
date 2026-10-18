<script lang="ts">
	import { onMount } from 'svelte';
	import LockKeyholeIcon from '@lucide/svelte/icons/lock-keyhole';
	import {
		consumeGuestHandoff,
		exchangeGuestSession,
		getGuestSession,
		endGuestSession,
		guestMediaUrl,
		guestDownloadUrl,
		type GuestSession,
		type GuestAccessStatus
	} from '$lib/guest-api';

	type View =
		| { kind: 'loading' }
		| { kind: 'session'; value: GuestSession }
		| { kind: 'error' }
		| { kind: 'closed' };
	let view = $state.raw<View>({ kind: 'loading' });
	let video = $state<HTMLVideoElement | undefined>();
	let boundSession = $state<string | undefined>();
	let cookieResumeAllowed = $state(false);
	let mediaError = $state(false);
	let endError = $state(false);
	let generation = 0;
	let controller: AbortController | null = null;
	let pollTimer: ReturnType<typeof setTimeout> | null = null;
	let ready = $derived(
		view.kind === 'session' && view.value.status === 'ready' ? view.value : null
	);
	let canRetry = $derived(boundSession !== undefined || cookieResumeAllowed);
	const accessDetails: Record<GuestAccessStatus, { heading: string; message: string }> = {
		expired: {
			heading: 'This review has ended.',
			message: 'The link expired. Playback has stopped.'
		},
		revoked: {
			heading: 'This review has ended.',
			message: 'The link was revoked. Playback has stopped.'
		},
		limit_reached: {
			heading: 'This review has ended.',
			message: 'The review limit has been reached. Playback has stopped.'
		},
		unavailable: {
			heading: 'This link is unavailable.',
			message: 'The link may have expired, been revoked, or no longer have evidence available.'
		}
	};
	let access = $derived(
		view.kind === 'session' && view.value.status !== 'ready'
			? accessDetails[view.value.status]
			: null
	);

	function clearMedia(): void {
		if (!video) return;
		video.pause();
		video.removeAttribute('src');
		video.load();
	}

	function cancelWork(): void {
		generation += 1;
		controller?.abort();
		controller = null;
		if (pollTimer !== null) clearTimeout(pollTimer);
		pollTimer = null;
	}

	function beginRequest(): { version: number; signal: AbortSignal } {
		cancelWork();
		clearMedia();
		mediaError = false;
		endError = false;
		view = { kind: 'loading' };
		controller = new AbortController();
		return { version: generation, signal: controller.signal };
	}

	function owns(version: number, signal: AbortSignal): boolean {
		return version === generation && !signal.aborted;
	}

	function acceptSession(value: GuestSession): void {
		if (value.status === 'ready') {
			if (boundSession !== undefined && value.session_id !== boundSession) {
				clearMedia();
				view = { kind: 'session', value: { status: 'unavailable' } };
				return;
			}
			boundSession = value.session_id;
			view = { kind: 'session', value };
			pollTimer = setTimeout(() => void pollSession(), 1_000);
		} else {
			clearMedia();
			view = { kind: 'session', value };
		}
	}

	async function openHandoff(): Promise<void> {
		const work = beginRequest();
		boundSession = undefined;
		cookieResumeAllowed = false;
		const handoff = consumeGuestHandoff();
		if (handoff.kind === 'invalid') {
			acceptSession({ status: 'unavailable' });
			return;
		}
		cookieResumeAllowed = handoff.kind === 'none';
		try {
			const value =
				handoff.kind === 'token'
					? await exchangeGuestSession(handoff.token, work.signal)
					: await getGuestSession(undefined, work.signal);
			if (owns(work.version, work.signal)) acceptSession(value);
		} catch {
			if (owns(work.version, work.signal)) view = { kind: 'error' };
		}
	}

	async function pollSession(): Promise<void> {
		pollTimer = null;
		if (!controller || boundSession === undefined) return;
		const signal = controller.signal;
		const version = generation;
		try {
			const value = await getGuestSession(boundSession, signal);
			if (owns(version, signal)) acceptSession(value);
		} catch {
			if (!owns(version, signal)) return;
			clearMedia();
			view = { kind: 'error' };
		}
	}

	async function retrySession(): Promise<void> {
		if (!canRetry) return;
		const work = beginRequest();
		try {
			const value = await getGuestSession(boundSession, work.signal);
			if (owns(work.version, work.signal)) acceptSession(value);
		} catch {
			if (owns(work.version, work.signal)) view = { kind: 'error' };
		}
	}

	async function closeReview(): Promise<void> {
		const sessionId = boundSession;
		cancelWork();
		clearMedia();
		view = { kind: 'closed' };
		if (sessionId === undefined) return;
		controller = new AbortController();
		const signal = controller.signal;
		const version = generation;
		try {
			await endGuestSession(sessionId, signal);
		} catch {
			if (owns(version, signal)) endError = true;
		}
	}

	function failedMedia(event: Event): void {
		if (!ready || event.currentTarget !== video) return;
		mediaError = true;
		clearMedia();
	}

	function formatTime(value: number): string {
		return (
			new Intl.DateTimeFormat('en-GB', {
				dateStyle: 'medium',
				timeStyle: 'medium',
				timeZone: 'UTC'
			}).format(value) + ' UTC'
		);
	}

	onMount(() => {
		const open = () => void openHandoff();
		window.addEventListener('keeppeek-guest-handoff', open);
		open();
		return () => {
			window.removeEventListener('keeppeek-guest-handoff', open);
			cancelWork();
			clearMedia();
		};
	});
</script>

<svelte:head><title>Shared review - KeepPeek</title></svelte:head>

<div class="min-h-svh bg-background text-foreground">
	<header class="flex items-center justify-between gap-4 border-b border-border px-4 py-4 md:px-8">
		<span class="text-base font-bold">KeepPeek</span>
		<span class="font-mono text-2xs tracking-caps text-primary-soft">SHARED EVIDENCE</span>
	</header>
	<main class="mx-auto max-w-[1000px] space-y-5 p-4 md:p-8" aria-busy={view.kind === 'loading'}>
		{#if ready}
			<h1 class="text-2xl font-semibold tracking-tight">
				{ready.display_name || 'Shared footage'}
			</h1>
			<p class="font-mono text-xs text-muted-foreground">
				{formatTime(ready.start_ms)} - {formatTime(ready.end_ms)}
			</p>
			{#if ready.can_stream && !mediaError}
				<video
					bind:this={video}
					src={guestMediaUrl(ready.session_id)}
					controls
					playsinline
					muted
					preload="none"
					controlslist="nodownload noremoteplayback"
					aria-label="Shared footage playback"
					class="aspect-video w-full rounded-sm border border-border bg-video object-contain"
					onerror={failedMedia}
				></video>
			{:else if mediaError}
				<div class="space-y-3 rounded-sm border border-border bg-card p-4">
					<p role="alert">The shared footage could not play. Check access and try again.</p>
					<button
						type="button"
						class="min-h-11 rounded-sm border border-border px-4 text-sm focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
						onclick={() => void retrySession()}>Retry playback</button
					>
				</div>
			{:else}
				<p class="text-sm text-muted-foreground">This link permits downloading the clip only.</p>
			{/if}
			{#if ready.has_gaps}<p role="status" class="text-sm text-activity">
					This clip contains gaps in recorded coverage.
				</p>{/if}
			<p class="text-sm text-muted-foreground">
				Shared for review until {formatTime(ready.expires_at_ms)}. Only this clip is available.
			</p>
			{#if ready.can_download}
				<a
					href={guestDownloadUrl(ready.session_id)}
					download
					class="inline-flex min-h-11 items-center justify-center rounded-sm bg-primary px-4 text-sm font-semibold text-primary-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					>Download MP4</a
				>
			{:else}
				<p class="text-xs text-muted-foreground">Download not permitted</p>
			{/if}
			<p class="text-xs text-muted-foreground">
				No account needed. This page cannot open other footage or live cameras.
			</p>
		{:else if view.kind === 'loading'}
			<h1 class="text-2xl font-semibold">Opening shared review...</h1>
			<p role="status" class="text-sm text-muted-foreground">Checking this link's access.</p>
		{:else if view.kind === 'closed'}
			<h1 class="text-2xl font-semibold">Review closed.</h1>
			<p class="text-sm text-muted-foreground">Playback has stopped. You can close this tab.</p>
			{#if endError}<p role="alert" class="text-sm text-destructive">
					The server could not confirm the session ended. Playback remains closed here.
				</p>{/if}
		{:else if view.kind === 'error'}
			<h1 class="text-2xl font-semibold">The shared review could not open.</h1>
			<p role="alert" class="text-sm text-muted-foreground">Check your connection and try again.</p>
			{#if canRetry}
				<button
					type="button"
					class="min-h-11 rounded-sm border border-border px-4 text-sm focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
					onclick={() => void retrySession()}>Try again</button
				>
			{:else}<p class="text-sm text-muted-foreground">
					Open the original link again to try. This page does not keep the link secret.
				</p>{/if}
		{:else if access}
			<section
				class="flex min-h-[28rem] flex-col justify-center gap-6 py-8"
				aria-labelledby="guest-access-heading"
			>
				<LockKeyholeIcon class="size-9 text-primary-soft" strokeWidth={1.5} aria-hidden="true" />
				<h1 id="guest-access-heading" class="text-2xl font-semibold tracking-tight">
					{access.heading}
				</h1>
				<p class="text-sm text-muted-foreground">{access.message}</p>
				<p class="text-sm text-muted-foreground">
					Ask the person who shared it for a new link. No other footage is available from this page.
				</p>
			</section>
		{/if}
		{#if view.kind !== 'closed'}
			<button
				type="button"
				class="min-h-11 rounded-sm bg-primary px-4 text-sm font-semibold text-primary-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
				onclick={() => void closeReview()}>{ready ? 'End review' : 'Close review'}</button
			>
		{/if}
	</main>
</div>
