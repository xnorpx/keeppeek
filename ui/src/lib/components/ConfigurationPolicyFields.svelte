<script lang="ts">
	import type { PolicyPatchDraft } from '$lib/configuration-editor';
	import { Input } from '$lib/components/ui/input/index.js';

	type Props = {
		draft: PolicyPatchDraft;
		includeCredentials?: boolean;
		includePorts?: boolean;
		clearLabel?: string;
	};

	let {
		draft = $bindable(),
		includeCredentials = false,
		includePorts = true,
		clearLabel = 'Use inherited value'
	}: Props = $props();

	const selectClass =
		'h-9 min-w-0 rounded-sm border border-hairline-strong bg-raised px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring';
</script>

<div class="grid gap-4">
	{#if includeCredentials}
		<div class="grid gap-2 sm:grid-cols-[minmax(0,1fr)_150px_minmax(180px,1fr)] sm:items-end">
			<label class="grid gap-1 text-sm font-medium" for="policy-username-operation">
				Username
				<select
					id="policy-username-operation"
					class={selectClass}
					bind:value={draft.username_operation}
				>
					<option value="unchanged">No change</option>
					<option value="set">Set reference</option>
					<option value="clear">{clearLabel}</option>
				</select>
			</label>
			<div class="hidden sm:block"></div>
			<label class="grid gap-1 text-sm font-medium" for="policy-username-reference">
				Secret reference
				<Input
					id="policy-username-reference"
					bind:value={draft.username_secret_reference}
					placeholder={'{secret:CAMERA_USERNAME}'}
					disabled={draft.username_operation !== 'set'}
					autocomplete="off"
				/>
			</label>
		</div>
		<div class="grid gap-2 sm:grid-cols-[minmax(0,1fr)_150px_minmax(180px,1fr)] sm:items-end">
			<label class="grid gap-1 text-sm font-medium" for="policy-password-operation">
				Password
				<select
					id="policy-password-operation"
					class={selectClass}
					bind:value={draft.password_operation}
				>
					<option value="unchanged">No change</option>
					<option value="set">Set reference</option>
					<option value="clear">{clearLabel}</option>
				</select>
			</label>
			<div class="hidden sm:block"></div>
			<label class="grid gap-1 text-sm font-medium" for="policy-password-reference">
				Secret reference
				<Input
					id="policy-password-reference"
					type="password"
					bind:value={draft.password_secret_reference}
					placeholder={'{secret:CAMERA_PASSWORD}'}
					disabled={draft.password_operation !== 'set'}
					autocomplete="new-password"
				/>
			</label>
		</div>
	{/if}

	{#if includePorts}
		<div class="grid gap-3 sm:grid-cols-2">
			<div class="grid gap-2 sm:grid-cols-[minmax(0,1fr)_120px] sm:items-end">
				<label class="grid gap-1 text-sm font-medium" for="policy-onvif-operation">
					ONVIF port
					<select
						id="policy-onvif-operation"
						class={selectClass}
						bind:value={draft.onvif_port_operation}
					>
						<option value="unchanged">No change</option>
						<option value="set">Set value</option>
						<option value="clear">Automatic</option>
					</select>
				</label>
				<label class="grid gap-1 text-sm font-medium" for="policy-onvif-value">
					Port
					<Input
						id="policy-onvif-value"
						type="number"
						min="1"
						max="65535"
						bind:value={draft.onvif_port}
						disabled={draft.onvif_port_operation !== 'set'}
					/>
				</label>
			</div>
			<div class="grid gap-2 sm:grid-cols-[minmax(0,1fr)_120px] sm:items-end">
				<label class="grid gap-1 text-sm font-medium" for="policy-http-operation">
					HTTP port
					<select
						id="policy-http-operation"
						class={selectClass}
						bind:value={draft.http_port_operation}
					>
						<option value="unchanged">No change</option>
						<option value="set">Set value</option>
						<option value="clear">Automatic</option>
					</select>
				</label>
				<label class="grid gap-1 text-sm font-medium" for="policy-http-value">
					Port
					<Input
						id="policy-http-value"
						type="number"
						min="1"
						max="65535"
						bind:value={draft.http_port}
						disabled={draft.http_port_operation !== 'set'}
					/>
				</label>
			</div>
		</div>
	{/if}

	<div class="grid gap-3 sm:grid-cols-2">
		<div class="grid gap-2 sm:grid-cols-[minmax(0,1fr)_150px] sm:items-end">
			<label class="grid gap-1 text-sm font-medium" for="policy-backend-operation">
				Backend
				<select
					id="policy-backend-operation"
					class={selectClass}
					bind:value={draft.backend_operation}
				>
					<option value="unchanged">No change</option>
					<option value="set">Set value</option>
					<option value="clear">{clearLabel}</option>
				</select>
			</label>
			<label class="grid gap-1 text-sm font-medium" for="policy-backend-value">
				Value
				<select
					id="policy-backend-value"
					class={selectClass}
					bind:value={draft.backend}
					disabled={draft.backend_operation !== 'set'}
				>
					<option value="auto">Auto</option>
					<option value="retina">Retina</option>
					<option value="reo-proto">Reo-Proto</option>
				</select>
			</label>
		</div>
		<div class="grid gap-2 sm:grid-cols-[minmax(0,1fr)_150px] sm:items-end">
			<label class="grid gap-1 text-sm font-medium" for="policy-transport-operation">
				Transport
				<select
					id="policy-transport-operation"
					class={selectClass}
					bind:value={draft.transport_operation}
				>
					<option value="unchanged">No change</option>
					<option value="set">Set value</option>
					<option value="clear">{clearLabel}</option>
				</select>
			</label>
			<label class="grid gap-1 text-sm font-medium" for="policy-transport-value">
				Value
				<select
					id="policy-transport-value"
					class={selectClass}
					bind:value={draft.transport}
					disabled={draft.transport_operation !== 'set'}
				>
					<option value="tcp">TCP</option>
					<option value="udp">UDP</option>
				</select>
			</label>
		</div>
	</div>

	<div class="grid gap-3 sm:grid-cols-2">
		<div class="grid gap-2 sm:grid-cols-[minmax(0,1fr)_150px] sm:items-end">
			<label class="grid gap-1 text-sm font-medium" for="policy-recording-operation">
				Recording mode
				<select
					id="policy-recording-operation"
					class={selectClass}
					bind:value={draft.recording_mode_operation}
				>
					<option value="unchanged">No change</option>
					<option value="set">Set value</option>
					<option value="clear">{clearLabel}</option>
				</select>
			</label>
			<label class="grid gap-1 text-sm font-medium" for="policy-recording-value">
				Value
				<select
					id="policy-recording-value"
					class={selectClass}
					bind:value={draft.recording_mode}
					disabled={draft.recording_mode_operation !== 'set'}
				>
					<option value="off">Off</option>
					<option value="sub">Sub</option>
					<option value="main">Main</option>
					<option value="both">Both</option>
					<option value="event-boost">Event boost</option>
				</select>
			</label>
		</div>
		<div class="grid gap-2 sm:grid-cols-[minmax(0,1fr)_150px] sm:items-end">
			<label class="grid gap-1 text-sm font-medium" for="policy-duration-operation">
				Event window
				<select
					id="policy-duration-operation"
					class={selectClass}
					bind:value={draft.event_recording_duration_secs_operation}
				>
					<option value="unchanged">No change</option>
					<option value="set">Set seconds</option>
					<option value="clear">{clearLabel}</option>
				</select>
			</label>
			<label class="grid gap-1 text-sm font-medium" for="policy-duration-value">
				Seconds
				<Input
					id="policy-duration-value"
					type="number"
					min="1"
					max="3600"
					bind:value={draft.event_recording_duration_secs}
					disabled={draft.event_recording_duration_secs_operation !== 'set'}
				/>
			</label>
		</div>
	</div>

	<div class="grid gap-2 sm:grid-cols-[minmax(0,1fr)_150px] sm:items-end">
		<label class="grid gap-1 text-sm font-medium" for="policy-motion-operation">
			Generic motion events
			<select
				id="policy-motion-operation"
				class={selectClass}
				bind:value={draft.record_generic_motion_events_operation}
			>
				<option value="unchanged">No change</option>
				<option value="set">Set value</option>
				<option value="clear">{clearLabel}</option>
			</select>
		</label>
		<label class="grid gap-1 text-sm font-medium" for="policy-motion-value">
			Value
			<select
				id="policy-motion-value"
				class={selectClass}
				bind:value={draft.record_generic_motion_events}
				disabled={draft.record_generic_motion_events_operation !== 'set'}
			>
				<option value={false}>Do not store</option>
				<option value={true}>Store</option>
			</select>
		</label>
	</div>
</div>
