<script lang="ts">
	import type { CameraWizardDraft } from '$lib/camera-wizard';
	import DesktopCameraWizardStreamsStep from '$lib/components/DesktopCameraWizardStreamsStep.svelte';
	import ArrowRightIcon from '@lucide/svelte/icons/arrow-right';
	import CheckIcon from '@lucide/svelte/icons/check';

	const draft = {
		ip: '192.168.1.71',
		displayName: 'Side Gate',
		username: 'admin',
		password: 'write-only-password',
		onvifPort: '8000',
		httpPort: '80',
		mainRtspUrl: 'rtsp://192.168.1.71/main',
		subRtspUrl: 'rtsp://192.168.1.71/sub',
		backend: 'reo-proto',
		transport: 'tcp',
		discoveryEvidence: 'ONVIF · DS-2CD2387G2'
	} satisfies CameraWizardDraft;

	const steps = [
		{ number: 1, label: 'Find', detail: '192.168.1.71 · ONVIF', state: 'complete' },
		{ number: 2, label: 'Connect', detail: 'CREDENTIAL DRAFT READY', state: 'complete' },
		{ number: 3, label: 'Streams', detail: '2 DECLARATIONS · UNPROBED', state: 'active' },
		{ number: 4, label: 'Recording', detail: '', state: 'pending' },
		{ number: 5, label: 'Review & save', detail: '', state: 'pending' }
	] as const;
</script>

<main
	data-paper-scenario="cameras.desktop.add-wizard"
	class="flex h-[685px] w-[1440px] overflow-hidden rounded-lg border border-hairline-strong bg-surface [font-synthesis:none]"
>
	<aside
		data-camera-wizard-stepper
		class="flex h-[683px] w-[300px] shrink-0 flex-col gap-0.5 border-r border-hairline bg-ground px-6 py-[26px]"
		aria-label="Add camera progress"
	>
		<h1 class="h-6 text-xl leading-6 font-semibold">Add a camera</h1>
		<div class="h-[38px] shrink-0 pb-6">
			<p class="font-mono text-2xs leading-[14px] tracking-[0.1em] text-text-faint">
				NOTHING SAVED UNTIL STEP 5
			</p>
		</div>
		{#each steps as step (step.number)}
			<div class="flex h-14 shrink-0 items-start gap-3">
				<span
					class="grid size-[22px] shrink-0 place-items-center rounded-full font-mono text-2xs {step.state ===
					'complete'
						? 'bg-healthy text-ground'
						: step.state === 'active'
							? 'bg-primary font-semibold text-on-primary'
							: 'border border-hairline-strong text-text-faint'}"
				>
					{#if step.state === 'complete'}<CheckIcon
							class="size-3"
							strokeWidth={3.2}
						/>{:else}{step.number}{/if}
				</span>
				<div class="flex min-w-0 flex-col gap-0.5">
					<p
						class="text-sm leading-[18px] {step.state === 'active'
							? 'font-semibold text-foreground'
							: step.state === 'complete'
								? 'font-medium text-text-muted'
								: 'font-medium text-text-faint'}"
					>
						{step.number} · {step.label}
					</p>
					{#if step.detail}<p
							class="font-mono text-2xs leading-[14px] {step.state === 'active'
								? 'text-primary-soft'
								: 'text-text-faint'}"
						>
							{step.detail}
						</p>{/if}
				</div>
			</div>
		{/each}
	</aside>

	<section data-camera-wizard-step-body class="flex h-[683px] w-[1140px] shrink-0 flex-col">
		<header
			class="flex h-[129px] w-[1140px] shrink-0 items-end justify-between border-b border-hairline px-7 pt-[26px] pb-[18px]"
		>
			<div class="flex w-[760px] shrink-0 flex-col gap-1.5">
				<h2 class="text-[28px] leading-[34px] font-semibold">Stream declarations</h2>
				<p class="text-sm leading-[22px] text-text-muted">
					Assign one recording stream and one live stream. The current server cannot authenticate or
					decode a candidate before the final configuration write.
				</p>
			</div>
			<button
				type="button"
				class="h-[30px] rounded-sm border border-hairline-strong px-3 text-[13px] text-text-faint"
				disabled>Re-probe unavailable</button
			>
		</header>

		<DesktopCameraWizardStreamsStep {draft} paperFrame />

		<footer
			class="flex h-[73px] w-[1140px] shrink-0 items-center justify-between border-t border-hairline px-7 py-[18px]"
		>
			<p class="font-mono text-2xs leading-[14px] tracking-[0.1em] text-text-faint">
				STEP 3 OF 5 · ESC DISCARDS EVERYTHING
			</p>
			<div class="flex items-center gap-2.5">
				<button
					type="button"
					class="h-9 rounded-sm border border-hairline-strong px-4 text-sm text-text-muted"
				>
					Back
				</button>
				<button
					type="button"
					class="inline-flex h-9 items-center gap-2 rounded-sm bg-primary px-5 text-sm font-semibold text-on-primary"
				>
					Continue to recording <ArrowRightIcon class="size-3.5" />
				</button>
			</div>
		</footer>
	</section>
</main>
