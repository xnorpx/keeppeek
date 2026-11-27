<script lang="ts">
	import { create } from '@bufbuild/protobuf';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import GuestShareDialog from '$lib/components/GuestShareDialog.svelte';
	import GuestShareGrants from '$lib/components/GuestShareGrants.svelte';
	import GuestSharingPolicy from '$lib/components/GuestSharingPolicy.svelte';
	import type { ShareCreateInput, SharePolicyInput } from '$lib/control-client-sharing';
	import {
		IssuedShareGrantSchema,
		ShareEvidenceSchema,
		ShareGrantListSchema,
		ShareGrantSchema,
		ShareGrantStatus,
		SharePolicySchema,
		type IssuedShareGrant,
		type ShareEvidence,
		type ShareGrant,
		type ShareGrantList,
		type SharePolicy
	} from '$lib/proto/webrtc_pb';
	import { shareTime } from '$lib/sharing';
	import CheckCircleIcon from '@lucide/svelte/icons/circle-check';
	import LinkIcon from '@lucide/svelte/icons/link';
	import LockKeyholeIcon from '@lucide/svelte/icons/lock-keyhole';
	import PlayIcon from '@lucide/svelte/icons/play';

	type Stage = 'policy' | 'export' | 'guest' | 'grants';

	const demoToken = 'Kp9xR4mN2vQ8wL5tB7yC3zF6hJ1dG0sA4eW8uP2kM9n';
	const checksum = 'a41f9c2e81b40d5c3e9a7b2f6c1d8e0a4f5b6c7d8e9f0a1b2c3d4e5f7d1304b8';
	const clipStartMs = BigInt(Date.UTC(2026, 7, 18, 6, 11, 48));
	const clipEndMs = BigInt(Date.UTC(2026, 7, 18, 6, 13, 18));
	const fileExpiryMs = BigInt(Date.now() + 18 * 3_600_000);

	const evidence: ShareEvidence = create(ShareEvidenceSchema, {
		exportJobId: 'export-front-door-01',
		artifactId: 'artifact-front-door-01',
		sha256: checksum,
		sourceId: 'front-door',
		sourceIdentity: '00000000-0000-4000-8000-000000000133',
		cameraName: 'Front Door',
		startMs: clipStartMs,
		endMs: clipEndMs,
		bytes: 38_400_000n,
		availableUntilMs: fileExpiryMs,
		hasGaps: false
	});

	let policyState = create(SharePolicySchema, {
		settings: { enabled: false, publicOrigin: '' },
		revision: 'policy-1',
		available: true
	});
	let storedGrants: ShareGrant[] = [];

	let stage = $state<Stage>('policy');
	let dialogOpen = $state(false);
	let guestClosed = $state(false);
	let guestClipLabel = $state('Delivery arrival — Front Door');
	let guestExpiresAtMs = $state(BigInt(Date.now() + 3_600_000));

	let policySaved = $state(false);
	let linkCreated = $state(false);
	let guestReviewEnded = $state(false);
	let grantRevoked = $state(false);

	let demoComplete = $derived(policySaved && linkCreated && guestReviewEnded && grantRevoked);

	const sharingController = {
		async getPolicy(): Promise<SharePolicy> {
			return policyState;
		},
		async configurePolicy(
			settings: SharePolicyInput,
			_expectedRevision: string
		): Promise<SharePolicy> {
			policyState = create(SharePolicySchema, {
				settings: {
					enabled: settings.enabled ?? false,
					publicOrigin: settings.publicOrigin ?? ''
				},
				revision: 'policy-2',
				available: true
			});
			policySaved = Boolean(policyState.settings?.enabled && policyState.settings.publicOrigin);
			return policyState;
		},
		async previewExport(_exportJobId: string): Promise<ShareEvidence> {
			return evidence;
		},
		async create(input: ShareCreateInput, expectedOrigin: string): Promise<IssuedShareGrant> {
			const grant = create(ShareGrantSchema, {
				grantId: input.requestId || '00000000-0000-4000-8000-000000000132',
				revision: 1n,
				creatorId: 'local-administrator',
				displayName: input.displayName || 'Front Door clip',
				note: input.note ?? '',
				createdAtMs: BigInt(Date.now()),
				expiresAtMs: input.expiresAtMs ?? BigInt(Date.now() + 3_600_000),
				status: ShareGrantStatus.ACTIVE,
				allowStream: input.allowStream ?? true,
				allowDownload: input.allowDownload ?? false,
				maxViews: input.maxViews,
				maxDownloads: input.maxDownloads,
				views: 0,
				downloads: 0,
				evidence
			});
			storedGrants = [grant, ...storedGrants.filter((item) => item.grantId !== grant.grantId)];
			guestClipLabel = grant.displayName;
			guestExpiresAtMs = grant.expiresAtMs;
			linkCreated = true;
			return create(IssuedShareGrantSchema, {
				grant,
				url: `${expectedOrigin}/share#${demoToken}`
			});
		},
		async list(): Promise<ShareGrantList> {
			return create(ShareGrantListSchema, { grants: storedGrants });
		},
		async revoke(
			grant: Pick<ShareGrant, 'grantId' | 'revision'>,
			reason: string
		): Promise<ShareGrant> {
			const target = storedGrants.find((item) => item.grantId === grant.grantId);
			if (!target) throw new Error('Grant not found.');
			const updated = create(ShareGrantSchema, {
				...target,
				revision: target.revision + 1n,
				status: ShareGrantStatus.REVOKED,
				revokeReason: reason
			});
			storedGrants = storedGrants.map((item) =>
				item.grantId === updated.grantId ? updated : item
			);
			grantRevoked = true;
			return updated;
		},
		async revokeEvidence(): Promise<number> {
			return storedGrants.length;
		}
	};

	function openGuestView(): void {
		stage = 'guest';
		guestClosed = false;
		storedGrants = storedGrants.map((grant) =>
			grant.views === 0
				? create(ShareGrantSchema, {
						...grant,
						views: 1,
						lastAccessAtMs: BigInt(Date.now())
					})
				: grant
		);
	}

	function endGuestReview(): void {
		guestClosed = true;
		guestReviewEnded = true;
	}

	const stages: readonly { id: Stage; label: string }[] = [
		{ id: 'policy', label: '1. Guest sharing policy' },
		{ id: 'export', label: '2. Ready export & link' },
		{ id: 'guest', label: '3. Guest /share view' },
		{ id: 'grants', label: '4. Guest links & revoke' }
	];
</script>

<main
	data-paper-scenario="keep.desktop.export-lifecycle"
	data-demo-guest-review
	data-demo-guest-review-complete={demoComplete ? 'true' : undefined}
	class="flex h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	{#if stage !== 'guest'}
		<DesktopPaperRail active={stage === 'export' ? 'keep' : 'settings'} paperFull />
	{/if}

	<section class="flex min-h-0 min-w-0 flex-1 flex-col">
		<header
			class="flex h-14 shrink-0 items-center justify-between gap-4 border-b border-hairline bg-surface px-5"
		>
			<div class="flex items-center gap-3">
				{#if stage === 'guest'}
					<span class="text-base font-bold">KeepPeek</span>
					<span class="font-mono text-2xs tracking-caps text-primary-soft">SHARED EVIDENCE</span>
					<span class="rounded-sm border border-hairline bg-raised px-2 py-0.5 font-mono text-2xs">
						https://review.example/share
					</span>
				{:else}
					<h1 class="text-sm font-semibold">
						{stage === 'export'
							? 'Keep · Export evidence'
							: 'Settings · Access · Guest review links'}
					</h1>
				{/if}
			</div>

			<nav
				class="flex items-center gap-1 rounded-sm bg-raised p-1"
				aria-label="Guest review stages"
			>
				{#each stages as item (item.id)}
					<button
						type="button"
						data-demo-action={`stage-${item.id}`}
						aria-pressed={stage === item.id}
						class="h-8 rounded-xs px-3 text-xs font-medium transition-colors {stage === item.id
							? 'bg-primary text-on-primary'
							: 'text-text-muted hover:text-foreground'}"
						onclick={() => (item.id === 'guest' ? openGuestView() : (stage = item.id))}
					>
						{item.label}
					</button>
				{/each}
			</nav>
		</header>

		<div class="min-h-0 flex-1 overflow-y-auto p-6">
			{#if stage === 'policy'}
				<div data-demo-guest-policy class="mx-auto max-w-2xl space-y-4">
					<div>
						<p class="font-mono text-2xs tracking-caps text-primary-soft">
							SETTINGS · ACCESS · GUEST SHARING
						</p>
						<h2 class="mt-1 text-xl font-semibold">Prepare deployment origin</h2>
						<p class="mt-1 text-sm text-text-muted">
							Guest sharing defaults off. Configure a canonical external HTTPS origin with no path
							or trailing slash before issuing links.
						</p>
					</div>
					<GuestSharingPolicy controller={sharingController} />
				</div>
			{:else if stage === 'export'}
				<div class="mx-auto max-w-2xl space-y-5">
					<div>
						<p class="font-mono text-2xs tracking-caps text-primary-soft">
							KEEP · EXPORT · READY ARTIFACT
						</p>
						<h2 class="mt-1 text-xl font-semibold">Share a completed evidence export</h2>
						<p class="mt-1 text-sm text-text-muted">
							Guest links bind to the measured file bounds and SHA-256 digest of a retained local
							MP4 (up to 2 minutes and 512 MiB).
						</p>
					</div>

					<article class="space-y-4 rounded-md border border-hairline-strong bg-surface p-5">
						<div class="flex items-center justify-between border-b border-hairline pb-3">
							<div class="flex items-center gap-2">
								<CheckCircleIcon class="size-4 text-healthy" />
								<span class="font-mono text-2xs tracking-caps text-healthy">READY · LOCAL MP4</span>
							</div>
							<span class="font-mono text-2xs text-text-muted">export-front-door-01</span>
						</div>

						<div>
							<h3 class="text-lg font-semibold">Front Door · 1m 30s</h3>
							<p class="mt-1 font-mono text-xs text-text-faint">
								front-door_2026-08-18T06-11-48Z_90s.mp4
							</p>
						</div>

						<dl
							class="divide-y divide-hairline rounded-sm border border-hairline bg-raised text-xs"
						>
							<div class="flex items-center justify-between px-3.5 py-2.5">
								<dt class="font-mono text-2xs text-text-faint">MEASURED BOUNDS</dt>
								<dd class="font-mono">{shareTime(clipStartMs)} – {shareTime(clipEndMs)}</dd>
							</div>
							<div class="flex items-center justify-between px-3.5 py-2.5">
								<dt class="font-mono text-2xs text-text-faint">SIZE &amp; CODEC</dt>
								<dd class="font-mono">38.4 MB · H.264 + AAC · NO GAPS</dd>
							</div>
							<div class="flex items-center justify-between px-3.5 py-2.5">
								<dt class="font-mono text-2xs text-text-faint">SHA-256</dt>
								<dd class="font-mono">a41f9c2e · · · 7d1304b8</dd>
							</div>
							<div class="flex items-center justify-between px-3.5 py-2.5">
								<dt class="font-mono text-2xs text-text-faint">FILE EXPIRY</dt>
								<dd class="font-mono text-activity">{shareTime(fileExpiryMs)}</dd>
							</div>
						</dl>

						<div class="flex items-center gap-3">
							<button
								type="button"
								data-demo-action="open-share-dialog"
								class="inline-flex h-10 items-center gap-2 rounded-sm bg-primary px-4 text-sm font-semibold text-on-primary"
								onclick={() => (dialogOpen = true)}
							>
								<LinkIcon class="size-4" />
								Create guest link
							</button>
							{#if linkCreated}
								<span class="font-mono text-xs text-healthy">
									One-time link issued · secret cleared on close
								</span>
							{/if}
						</div>
					</article>
				</div>
			{:else if stage === 'guest'}
				<div class="mx-auto max-w-[940px] space-y-4">
					{#if !guestClosed}
						<div class="flex items-start justify-between gap-4">
							<div>
								<h1 class="text-2xl font-semibold tracking-tight">{guestClipLabel}</h1>
								<p class="mt-1 font-mono text-xs text-text-muted">
									{shareTime(clipStartMs)} - {shareTime(clipEndMs)}
								</p>
							</div>
							<span
								class="inline-flex items-center gap-1.5 rounded-sm border border-primary/40 bg-primary/10 px-2.5 py-1 font-mono text-2xs text-primary-soft"
							>
								<LockKeyholeIcon class="size-3.5" />
								ISOLATED /share SESSION · NO APP SHELL
							</span>
						</div>

						<div
							class="overflow-hidden rounded-sm border border-hairline bg-video"
							aria-label="Shared footage playback"
						>
							<svg
								viewBox="0 0 960 400"
								class="block w-full"
								role="img"
								aria-label="Isolated guest clip playback for Front Door"
							>
								<rect width="960" height="230" fill="#172336" />
								<rect y="230" width="960" height="170" fill="#1e2530" />
								<polygon points="270,230 740,230 920,400 80,400" fill="#273142" />
								<rect
									x="68"
									y="82"
									width="260"
									height="156"
									rx="4"
									fill="#2b3546"
									stroke="#475569"
									stroke-width="2"
								/>
								<rect
									x="510"
									y="192"
									width="226"
									height="112"
									rx="14"
									fill="#dbeafe"
									stroke="#64748b"
									stroke-width="2"
								/>
								<rect x="534" y="210" width="178" height="42" rx="6" fill="#1e293b" />
								<rect x="586" y="268" width="74" height="22" rx="3" fill="#ffffff" />
								<text
									x="623"
									y="283"
									text-anchor="middle"
									fill="#0f172a"
									font-family="monospace"
									font-size="12"
									font-weight="700"
								>
									KP-4829
								</text>
								<rect
									x="20"
									y="18"
									width="318"
									height="32"
									rx="4"
									fill="#090c12"
									fill-opacity="0.78"
								/>
								<text x="32" y="39" fill="#f8fafc" font-family="monospace" font-size="14">
									2026-08-18 06:12:14 UTC · CLIP ONLY
								</text>
							</svg>
							<div
								class="flex h-10 items-center gap-3 border-t border-white/10 bg-black/80 px-4 font-mono text-2xs text-white/80"
							>
								<PlayIcon class="size-4 text-white" fill="currentColor" />
								<span>00:26 / 01:30</span>
								<span class="h-1 flex-1 rounded-full bg-white/20">
									<span class="block h-full w-1/3 rounded-full bg-primary"></span>
								</span>
								<span>1 VIEW COMMITTED</span>
							</div>
						</div>

						<div class="flex flex-wrap items-center justify-between gap-4">
							<div class="space-y-1 text-xs text-text-muted">
								<p>
									Shared for review until {shareTime(guestExpiresAtMs)}. Only this clip is
									available.
								</p>
								<p>
									Download not permitted · No account needed. This page cannot open other footage or
									live cameras.
								</p>
							</div>
							<button
								type="button"
								data-demo-action="guest-end-review"
								class="inline-flex h-10 items-center rounded-sm bg-primary px-4 text-sm font-semibold text-on-primary"
								onclick={endGuestReview}
							>
								End review
							</button>
						</div>
					{:else}
						<div class="rounded-md border border-hairline bg-surface p-8">
							<h1 class="text-2xl font-semibold">Review closed.</h1>
							<p class="mt-2 text-sm text-text-muted">
								Playback has stopped. You can close this tab. Ending a session clears the scoped
								guest cookie without revoking the link.
							</p>
						</div>
					{/if}
				</div>
			{:else}
				<div data-demo-guest-grants class="mx-auto max-w-3xl space-y-4">
					<div>
						<p class="font-mono text-2xs tracking-caps text-primary-soft">
							SETTINGS · ACCESS · GUEST LINKS
						</p>
						<h2 class="mt-1 text-xl font-semibold">Audit usage and revoke grants</h2>
					</div>
					<GuestShareGrants controller={sharingController} />
				</div>
			{/if}
		</div>
	</section>

	{#if dialogOpen}
		<GuestShareDialog
			controller={sharingController}
			exportJobId="export-front-door-01"
			onclose={() => (dialogOpen = false)}
		/>
	{/if}
</main>
