<script lang="ts">
	import { onMount } from 'svelte';
	import type { CameraListItem, ServerHealthResponse } from '$lib/types';
	import { getCameras, getServerHealth } from '$lib/api';
	import { useLivePeer } from '$lib/stream-peer-context';
	import * as Card from '$lib/components/ui/card/index.js';
	import { Badge } from '$lib/components/ui/badge/index.js';
	import { Skeleton } from '$lib/components/ui/skeleton/index.js';
	import LiveVideo from '$lib/components/LiveVideo.svelte';

	let cameras: CameraListItem[] = $state([]);
	let serverHealth = $state.raw<ServerHealthResponse | null>(null);
	let error: string | null = $state(null);
	let loading = $state(true);
	const livePeer = useLivePeer();
	let liveCameraIds = $derived(
		new Set(
			(serverHealth?.cameras ?? [])
				.filter((camera) => camera.state === 'online' || camera.state === 'degraded')
				.map((camera) => camera.id)
		)
	);
	let livePlans = $derived(
		cameras
			.filter((camera) => liveCameraIds.has(camera.id))
			.map((camera) => ({ cameraId: camera.id, quality: 'low' as const }))
	);

	$effect(() => {
		void livePeer.configure(livePlans).catch((error) => {
			console.error('Unable to configure shared live view', error);
		});
	});

	onMount(() => {
		loadCameras();
	});

	async function loadCameras() {
		try {
			const [nextCameras, nextServerHealth] = await Promise.all([getCameras(), getServerHealth()]);
			cameras = nextCameras;
			serverHealth = nextServerHealth;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load cameras';
		} finally {
			loading = false;
		}
	}

	function previewStream(camera: CameraListItem): 'main' | 'sub' {
		return (
			camera.profiles.find((profile) => profile.stream === 'sub' && profile.encoding === 'h264')
				?.stream ??
			camera.profiles.find((profile) => profile.encoding === 'h264')?.stream ??
			camera.profiles.at(-1)?.stream ??
			'main'
		);
	}
</script>

<svelte:head>
	<title>Cameras - KeepPeek</title>
</svelte:head>

<div class="space-y-6">
	<h1 class="text-2xl font-bold tracking-tight">Cameras</h1>

	{#if loading}
		<div class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
			{#each [0, 1, 2, 3, 4, 5] as skeleton (skeleton)}
				<Card.Root>
					<Card.Header>
						<Skeleton class="h-5 w-32" />
						<Skeleton class="h-4 w-24" />
					</Card.Header>
					<Card.Content class="space-y-2">
						<Skeleton class="aspect-video w-full rounded-md" />
						<Skeleton class="h-4 w-3/4" />
					</Card.Content>
				</Card.Root>
			{/each}
		</div>
	{:else if error}
		<Card.Root class="border-destructive">
			<Card.Header>
				<Card.Title>Error</Card.Title>
			</Card.Header>
			<Card.Content>
				<p class="text-sm text-destructive">{error}</p>
			</Card.Content>
		</Card.Root>
	{:else if cameras.length === 0}
		<Card.Root>
			<Card.Content class="py-12 text-center">
				<p class="text-muted-foreground">No cameras configured.</p>
				<p class="mt-1 text-sm text-muted-foreground">
					Add cameras to your configuration file to get started.
				</p>
			</Card.Content>
		</Card.Root>
	{:else}
		<div class="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
			{#each cameras as camera (camera.id)}
				<Card.Root class="overflow-hidden">
					<LiveVideo cameraId={camera.id} stream={previewStream(camera)} class="aspect-video" />
					<Card.Header>
						<div class="flex items-center justify-between">
							<Card.Title class="text-base">{camera.name ?? camera.id}</Card.Title>
							{#if camera.is_reolink}
								<Badge variant="secondary">Reolink</Badge>
							{/if}
						</div>
						{#if camera.name}
							<Card.Description>{camera.id}</Card.Description>
						{/if}
					</Card.Header>
					<Card.Content>
						<dl class="space-y-1 text-sm">
							<div class="flex justify-between">
								<dt class="text-muted-foreground">IP</dt>
								<dd class="font-mono text-xs">{camera.ip}</dd>
							</div>
							{#if camera.manufacturer}
								<div class="flex justify-between">
									<dt class="text-muted-foreground">Manufacturer</dt>
									<dd>{camera.manufacturer}</dd>
								</div>
							{/if}
							{#if camera.model}
								<div class="flex justify-between">
									<dt class="text-muted-foreground">Model</dt>
									<dd>{camera.model}</dd>
								</div>
							{/if}
							{#if camera.profiles.length > 0}
								<div class="flex flex-wrap gap-1 pt-1">
									{#each camera.profiles as profile (profile.stream)}
										<Badge variant="outline" class="text-xs">
											{profile.encoding ?? profile.name}
										</Badge>
									{/each}
								</div>
							{/if}
						</dl>
					</Card.Content>
				</Card.Root>
			{/each}
		</div>
	{/if}
</div>
