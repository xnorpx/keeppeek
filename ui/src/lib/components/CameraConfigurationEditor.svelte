<script lang="ts">
	import { tick, untrack } from 'svelte';
	import EventRecordingFields from './EventRecordingFields.svelte';
	import type {
		CameraBackend,
		CameraRecordingMode,
		EventRecordingStream,
		CameraSettings,
		CameraSettingsUpdate,
		CameraTransport
	} from '$lib/types';
	import { Button } from '$lib/components/ui/button/index.js';
	import { Input } from '$lib/components/ui/input/index.js';
	import RefreshCwIcon from '@lucide/svelte/icons/refresh-cw';
	import SaveIcon from '@lucide/svelte/icons/save';
	import XIcon from '@lucide/svelte/icons/x';

	type Form = {
		displayName: string;
		username: string;
		password: string;
		onvifPort: string;
		httpPort: string;
		mainRtspUrl: string;
		subRtspUrl: string;
		uid: string;
		removeUid: boolean;
		backend: CameraBackend;
		transport: CameraTransport;
		recordGenericMotionEvents: boolean;
		recordingMode: CameraRecordingMode;
		eventRecordingDurationSeconds: string;
		eventPreRecordingDurationSeconds: string;
		eventRecordingStream: EventRecordingStream;
	};

	type Props = {
		camera: CameraSettings;
		preRecordingSupported?: boolean;
		saving?: boolean;
		error?: string | null;
		oncancel: () => void;
		onsave: (update: CameraSettingsUpdate) => void | Promise<void>;
	};

	let {
		camera,
		preRecordingSupported = false,
		saving = false,
		error = null,
		oncancel,
		onsave
	}: Props = $props();
	let form = $state<Form>(untrack(() => formFromCamera(camera)));
	let errorSummary = $state<HTMLParagraphElement>();
	let validationError = $state<string | null>(null);

	const selectClass =
		'border-input bg-background ring-offset-background focus-visible:border-ring focus-visible:ring-ring/50 h-9 w-full rounded-md border px-3 text-sm font-medium shadow-xs outline-none focus-visible:ring-[3px]';

	function formFromCamera(value: CameraSettings): Form {
		return {
			displayName: value.display_name ?? '',
			username: '',
			password: '',
			onvifPort: value.onvif_port?.toString() ?? '',
			httpPort: value.http_port?.toString() ?? '',
			mainRtspUrl: value.main_rtsp_url ?? '',
			subRtspUrl: value.sub_rtsp_url ?? '',
			uid: '',
			removeUid: false,
			backend: value.backend,
			transport: value.transport,
			recordGenericMotionEvents: value.record_generic_motion_events,
			recordingMode: value.recording_mode,
			eventRecordingDurationSeconds: value.event_recording_duration_secs.toString(),
			eventPreRecordingDurationSeconds: (value.event_pre_recording_duration_secs ?? 0).toString(),
			eventRecordingStream: value.event_recording_stream ?? 'main'
		};
	}

	function parsePort(value: string, label: string): number | null {
		const normalized = value.trim();
		if (!normalized) return null;
		const port = Number(normalized);
		if (!Number.isInteger(port) || port < 1 || port > 65_535) {
			throw new Error(`${label} must be a whole number from 1 to 65535.`);
		}
		return port;
	}

	function parseEventDuration(): number {
		const normalized = form.eventRecordingDurationSeconds.trim();
		if (!/^\d+$/.test(normalized)) {
			throw new Error('Event recording duration must be a whole number.');
		}
		const duration = Number(normalized);
		if (duration < 1 || duration > 3_600) {
			throw new Error('Event recording duration must be between 1 and 3600 seconds.');
		}
		return duration;
	}

	function parsePreDuration(): number {
		const value = form.eventPreRecordingDurationSeconds.trim();
		const seconds = Number(value);
		if (!/^\d+$/.test(value) || !Number.isInteger(seconds) || seconds < 0 || seconds > 30) {
			throw new Error('Pre-recording duration must be a whole number from 0 to 30 seconds.');
		}
		return seconds;
	}

	function updateFromForm(): CameraSettingsUpdate {
		const update: CameraSettingsUpdate = {
			display_name: form.displayName.trim() || null,
			onvif_port: parsePort(form.onvifPort, 'ONVIF port'),
			http_port: parsePort(form.httpPort, 'HTTP port'),
			main_rtsp_url: form.mainRtspUrl.trim() || null,
			sub_rtsp_url: form.subRtspUrl.trim() || null,
			backend: form.backend,
			transport: form.transport,
			record_generic_motion_events: form.recordGenericMotionEvents,
			recording_mode: form.recordingMode,
			event_recording_duration_secs:
				form.recordingMode === 'event-boost' || form.recordingMode === 'event-only'
					? parseEventDuration()
					: camera.event_recording_duration_secs,
			...(preRecordingSupported
				? {
						event_pre_recording_duration_secs:
							form.recordingMode === 'event-boost' || form.recordingMode === 'event-only'
								? parsePreDuration()
								: (camera.event_pre_recording_duration_secs ?? 0),
						event_recording_stream: form.eventRecordingStream
					}
				: {})
		};
		if (form.username) update.username = form.username;
		if (form.password) update.password = form.password;
		if (form.uid.trim()) update.uid = form.uid.trim();
		else if (form.removeUid) update.uid = null;
		return update;
	}

	async function submit(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (saving) return;
		validationError = null;
		try {
			void onsave(updateFromForm());
		} catch (cause) {
			validationError = cause instanceof Error ? cause.message : 'Camera configuration is invalid.';
			await tick();
			errorSummary?.focus();
		}
	}
</script>

<form
	data-camera-configuration-editor
	class="scroll-mt-16 overflow-hidden rounded-md border border-hairline bg-surface"
	onsubmit={submit}
>
	<fieldset disabled={saving} class="contents">
		<header
			class="flex flex-wrap items-start justify-between gap-4 border-b border-hairline px-4 py-4"
		>
			<div>
				<p class="font-mono text-2xs tracking-caps text-primary-soft">CAMERA CONFIGURATION</p>
				<h2 class="mt-1 text-lg font-semibold">Edit camera settings</h2>
				<p class="mt-1 text-xs leading-5 text-text-muted">
					{camera.ip} · Blank username and password fields keep the current secret.
				</p>
			</div>
			<Button type="button" variant="ghost" size="sm" onclick={oncancel}>
				<XIcon /> Close
			</Button>
		</header>

		<div class="grid gap-5 p-4 lg:grid-cols-2">
			<section class="space-y-4" aria-labelledby="camera-identity-settings-heading">
				<h3 id="camera-identity-settings-heading" class="text-sm font-semibold">
					Identity and sign-in
				</h3>
				<label class="grid gap-1.5 text-sm font-medium" for="camera-config-display-name">
					Display name
					<Input id="camera-config-display-name" bind:value={form.displayName} autocomplete="off" />
				</label>
				<div class="grid gap-4 sm:grid-cols-2">
					<label class="grid gap-1.5 text-sm font-medium" for="camera-config-username">
						Username
						<Input
							id="camera-config-username"
							bind:value={form.username}
							placeholder={camera.username_configured ? 'Configured · enter to replace' : ''}
							autocomplete="username"
						/>
					</label>
					<label class="grid gap-1.5 text-sm font-medium" for="camera-config-password">
						Password
						<Input
							id="camera-config-password"
							type="password"
							bind:value={form.password}
							placeholder={camera.password_configured ? 'Configured · enter to replace' : ''}
							autocomplete="new-password"
						/>
					</label>
				</div>
				<label class="grid gap-1.5 text-sm font-medium" for="camera-config-uid">
					P2P UID
					<Input
						id="camera-config-uid"
						bind:value={form.uid}
						placeholder={camera.uid_configured ? 'Configured · enter to replace' : 'Optional'}
						autocomplete="off"
					/>
				</label>
				{#if camera.uid_configured}
					<label class="flex items-center gap-2 text-sm">
						<input type="checkbox" bind:checked={form.removeUid} class="size-4 accent-primary" />
						Remove stored P2P UID
					</label>
				{/if}
			</section>

			<section class="space-y-4" aria-labelledby="camera-connection-settings-heading">
				<h3 id="camera-connection-settings-heading" class="text-sm font-semibold">Connection</h3>
				<div class="grid gap-4 sm:grid-cols-2">
					<label class="grid gap-1.5 text-sm font-medium" for="camera-config-backend">
						Backend
						<select id="camera-config-backend" class={selectClass} bind:value={form.backend}>
							<option value="auto">Auto</option>
							<option value="retina">Retina RTSP</option>
							<option value="reo-proto">Reo-Proto</option>
						</select>
					</label>
					<label class="grid gap-1.5 text-sm font-medium" for="camera-config-transport">
						Transport
						<select id="camera-config-transport" class={selectClass} bind:value={form.transport}>
							<option value="tcp">TCP</option>
							<option value="udp">UDP</option>
						</select>
					</label>
					<label class="grid gap-1.5 text-sm font-medium" for="camera-config-onvif-port">
						ONVIF port
						<Input
							id="camera-config-onvif-port"
							bind:value={form.onvifPort}
							inputmode="numeric"
							autocomplete="off"
						/>
					</label>
					<label class="grid gap-1.5 text-sm font-medium" for="camera-config-http-port">
						HTTP port
						<Input
							id="camera-config-http-port"
							bind:value={form.httpPort}
							inputmode="numeric"
							autocomplete="off"
						/>
					</label>
				</div>
				<label class="grid gap-1.5 text-sm font-medium" for="camera-config-main-url">
					Main RTSP stream URL
					<Input id="camera-config-main-url" bind:value={form.mainRtspUrl} autocomplete="off" />
				</label>
				<label class="grid gap-1.5 text-sm font-medium" for="camera-config-sub-url">
					Sub RTSP stream URL
					<Input id="camera-config-sub-url" bind:value={form.subRtspUrl} autocomplete="off" />
				</label>
			</section>

			<section
				class="space-y-4 border-t border-hairline pt-5 lg:col-span-2"
				aria-labelledby="camera-recording-settings-heading"
			>
				<h3 id="camera-recording-settings-heading" class="text-sm font-semibold">Recording</h3>
				<div class="grid gap-4 md:grid-cols-2">
					<label class="grid gap-1.5 text-sm font-medium" for="camera-config-recording-mode">
						Recording mode
						<select
							id="camera-config-recording-mode"
							class={selectClass}
							bind:value={form.recordingMode}
						>
							<option value="event-boost">Sub, switch to main on events (recommended)</option>
							{#if preRecordingSupported}<option value="event-only">Events only</option>{/if}
							<option value="sub">Sub only</option>
							<option value="main">Main only</option>
							<option value="both">Main + sub</option>
							<option value="off">Don't record</option>
						</select>
					</label>
				</div>
				<EventRecordingFields
					{preRecordingSupported}
					mode={form.recordingMode}
					duration={form.eventRecordingDurationSeconds}
					preDuration={form.eventPreRecordingDurationSeconds}
					stream={form.eventRecordingStream}
					onduration={(value) => (form.eventRecordingDurationSeconds = value)}
					onpre={(value) => (form.eventPreRecordingDurationSeconds = value)}
					onstream={(value) => (form.eventRecordingStream = value)}
				/>
				<label class="flex items-start gap-3 rounded-sm border border-hairline bg-raised p-3">
					<input
						type="checkbox"
						bind:checked={form.recordGenericMotionEvents}
						class="mt-0.5 size-4 accent-primary"
					/>
					<span>
						<span class="block text-sm font-medium">Store generic motion events</span>
						<span class="mt-1 block text-xs leading-5 text-text-muted">
							Off stores classified person, animal, and vehicle alarms without generic motion
							snapshots.
						</span>
					</span>
				</label>
				<p class="text-xs leading-5 text-text-muted">
					Event boost records substream GOPs normally, switches to main on an event keyframe, then
					returns to sub after the configured window.
				</p>
			</section>
		</div>
	</fieldset>

	{#if validationError || error}
		<p bind:this={errorSummary} tabindex="-1" class="mx-4 text-sm text-destructive" role="alert">
			{validationError ?? error}
		</p>
	{/if}
	<footer class="mt-4 flex flex-wrap justify-end gap-2 border-t border-hairline px-4 py-4">
		<Button type="button" variant="outline" onclick={oncancel} disabled={saving}>Cancel</Button>
		<Button type="submit" disabled={saving}>
			{#if saving}<RefreshCwIcon class="animate-spin" />{:else}<SaveIcon />{/if}
			{saving ? 'Saving camera settings' : 'Save camera settings'}
		</Button>
	</footer>
</form>
