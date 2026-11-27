<script lang="ts">
	import { tick } from 'svelte';
	import { presentCameraFleetRow } from '$lib/camera-fleet';
	import { setCapabilityState } from '$lib/capability-context';
	import CameraFleetRow from '$lib/components/CameraFleetRow.svelte';
	import DesktopPaperRail from '$lib/components/DesktopPaperRail.svelte';
	import FleetConfigurationManager from '$lib/components/FleetConfigurationManager.svelte';
	import { Button } from '$lib/components/ui/button/index.js';
	import type { ControlClient } from '$lib/control-client';
	import { setControlClient } from '$lib/control-context';
	import type {
		CameraEffectiveConfiguration,
		CameraHealth,
		CameraHealthDimensions,
		CameraListItem,
		ConfigurationApplyResult,
		ConfigurationPlan,
		ConfigurationPlanRequest,
		ConfigurationSnapshot,
		ConfigurationTemplate,
		PrivacyStatus,
		StreamHealthDimensions
	} from '$lib/types';
	import SearchIcon from '@lucide/svelte/icons/search';
	import SlidersHorizontalIcon from '@lucide/svelte/icons/sliders-horizontal';

	setCapabilityState(['keeppeek.configuration.v1', 'keeppeek.recording.pre-roll.v1']);

	const cameraSpecs = [
		['front-door', 'Front Door', '192.0.2.41', 'Reolink', 'RLC-811A', 'reo-proto', 'tcp'],
		['porch', 'Porch', '192.0.2.42', 'ONVIF', 'DS-2CD2143', 'retina', 'udp'],
		['back-yard', 'Back Yard', '192.0.2.43', 'Reolink', 'RLC-820A', 'reo-proto', 'tcp'],
		['yard-ptz', 'Yard PTZ', '192.0.2.44', 'ONVIF', 'DS-2DE4425', 'retina', 'tcp'],
		['driveway', 'Driveway', '192.0.2.45', 'Reolink', 'RLC-811A', 'retina', 'udp'],
		['workshop', 'Workshop', '192.0.2.46', 'ONVIF', 'DS-2CD2143', 'retina', 'tcp']
	] as const;

	const cameras: CameraListItem[] = cameraSpecs.map(([id, name, ip, brand, model, backend]) => ({
		id,
		ip,
		name,
		manufacturer: brand,
		model,
		firmware_version: null,
		is_reolink: brand === 'Reolink',
		backend: backend === 'reo-proto' ? 'Reolink' : 'ONVIF',
		transport: backend === 'reo-proto' ? 'Baichuan · TCP' : 'RTSP · TCP',
		capabilities: {
			ptz: id === 'yard-ptz',
			audio: true,
			events: true,
			recording: true,
			analytics: false,
			imaging: true,
			two_way_audio: false
		},
		profiles: [
			{
				name: 'Main',
				stream: 'main',
				encoding: 'h265',
				resolution: '3840x2160',
				framerate: 25
			},
			{
				name: 'Sub',
				stream: 'sub',
				encoding: 'h264',
				resolution: '640x360',
				framerate: 15
			}
		]
	}));

	const healthById = new Map<string, CameraHealth>(
		cameras.map((camera, index) => {
			const state = index === 1 ? 'degraded' : index === 2 ? 'offline' : 'healthy';
			const reason =
				state === 'healthy'
					? 'healthy'
					: state === 'degraded'
						? 'ingress_drops'
						: 'transport_disconnected';
			const detail =
				state === 'healthy'
					? 'Transport, media, keyframe, and recording evidence is current'
					: state === 'degraded'
						? '14% frames dropped'
						: 'Connection refused';
			const current = state !== 'offline';
			return [
				camera.id,
				{
					id: camera.id,
					ip: camera.ip,
					name: camera.name ?? camera.id,
					manufacturer: camera.manufacturer,
					model: camera.model,
					firmware_version: null,
					backend: camera.backend,
					transport: camera.transport,
					state,
					reason,
					reason_codes: [reason],
					detail,
					dimensions: {
						transport_connected: current,
						frames_fresh: current,
						decodable: current,
						recording_requested: true,
						recording_progressing: current
					} as CameraHealthDimensions,
					lifecycle: state === 'offline' ? 'Reconnecting' : 'Connected',
					last_error: state === 'offline' ? 'Connection refused' : null,
					configured_profiles: camera.profiles,
					streams: [
						{
							type: 'main',
							codec: 'h265',
							resolution: '3840x2160',
							fps: state === 'offline' ? 0 : 25,
							kbps: state === 'offline' ? undefined : 6_200,
							frames: state === 'offline' ? 0 : 86_400,
							drops: index === 1 ? 14 : 0,
							updated_at_ms: 1,
							report_age_ms: state === 'offline' ? 8_040_000 : 20,
							state,
							reason,
							reason_codes: [reason],
							detail,
							dimensions: {
								expected: true,
								transport_connected: current,
								report_fresh: current,
								frames_fresh: current,
								decodable: current,
								recording_requested: true,
								recording_progressing: current
							} as StreamHealthDimensions
						}
					]
				}
			] as const;
		})
	);

	const rows = cameras.map((camera) => ({
		camera,
		presentation: presentCameraFleetRow(camera, healthById.get(camera.id) ?? null)
	}));

	const emptyPrivacyStatus: PrivacyStatus = {
		configured: false,
		active: false,
		enabled: false,
		timezone: 'UTC',
		next_transition_at_ms: null,
		configured_source: 'none',
		effective_source: 'none',
		override_expires_at_ms: null,
		override_actor: null,
		override_reason: null,
		blocked_capabilities: [],
		error: null,
		revision: 1
	};

	const perimeterTemplate: ConfigurationTemplate = {
		template_id: 'tpl-perimeter-event-boost',
		version: 2,
		name: 'Perimeter Event Boost',
		description:
			'Reo-Proto TCP with 10s pre-roll and {secret:CAMERA_PASSWORD} credential reference',
		values: {
			username_secret_reference: '{secret:CAMERA_USERNAME}',
			password_secret_reference: '{secret:CAMERA_PASSWORD}',
			backend: 'reo-proto',
			transport: 'tcp',
			recording_mode: 'event-boost',
			event_recording_duration_secs: 60,
			event_pre_recording_duration_secs: 10,
			event_recording_stream: 'main',
			record_generic_motion_events: false
		},
		created_at_ms: Date.parse('2026-08-01T10:00:00Z'),
		updated_at_ms: Date.parse('2026-08-18T05:30:00Z')
	};

	const indoorTemplate: ConfigurationTemplate = {
		template_id: 'tpl-indoor-sub',
		version: 1,
		name: 'Indoor Continuous Sub',
		description: 'Sub-stream continuous recording with {secret:INDOOR_PASSWORD}',
		values: {
			username_secret_reference: '{secret:INDOOR_USERNAME}',
			password_secret_reference: '{secret:INDOOR_PASSWORD}',
			backend: 'retina',
			transport: 'tcp',
			recording_mode: 'sub',
			event_recording_duration_secs: 30
		},
		created_at_ms: Date.parse('2026-08-05T12:00:00Z'),
		updated_at_ms: Date.parse('2026-08-12T08:00:00Z')
	};

	function makeCameraEvidence(
		id: string,
		displayName: string,
		ip: string,
		model: string,
		backend: 'reo-proto' | 'retina',
		transport: 'tcp' | 'udp',
		recordingMode: 'sub' | 'event-boost',
		recordingSource: 'default' | 'override',
		transportSource: 'default' | 'override'
	): CameraEffectiveConfiguration {
		return {
			camera: {
				id,
				ip,
				display_name: displayName,
				manufacturer_override: null,
				username_configured: true,
				password_configured: true,
				onvif_port: 8000,
				http_port: 80,
				main_rtsp_url: `rtsp://${ip}:554/h265Preview_01_main`,
				sub_rtsp_url: `rtsp://${ip}:554/h264Preview_01_sub`,
				uid_configured: false,
				backend,
				transport,
				record_generic_motion_events: false,
				recording_mode: recordingMode,
				event_recording_duration_secs: recordingMode === 'event-boost' ? 60 : 30,
				event_pre_recording_duration_secs: recordingMode === 'event-boost' ? 10 : 0,
				event_recording_stream: 'main',
				health: 'healthy',
				model
			},
			group_ids: ['perimeter'],
			username: {
				default_configured: true,
				override_configured: false,
				effective_configured: true,
				source: 'default',
				runtime_applied: true,
				warning: null
			},
			password: {
				default_configured: true,
				override_configured: false,
				effective_configured: true,
				source: 'default',
				runtime_applied: true,
				warning: null
			},
			backend: {
				configured_default: 'retina',
				camera_override: backend === 'retina' ? null : backend,
				effective: backend,
				source: backend === 'retina' ? 'default' : 'override',
				runtime_applied: true,
				warning: null
			},
			transport: {
				configured_default: 'tcp',
				camera_override: transportSource === 'override' ? transport : null,
				effective: transport,
				source: transportSource,
				runtime_applied: true,
				warning: null
			},
			record_generic_motion_events: {
				configured_default: false,
				camera_override: null,
				effective: false,
				source: 'default',
				runtime_applied: true,
				warning: null
			},
			recording_mode: {
				configured_default: 'sub',
				camera_override: recordingSource === 'override' ? recordingMode : null,
				effective: recordingMode,
				source: recordingSource,
				runtime_applied: true,
				warning: null
			},
			event_recording_duration_secs: {
				configured_default: 30,
				camera_override: recordingMode === 'event-boost' ? 60 : null,
				effective: recordingMode === 'event-boost' ? 60 : 30,
				source: recordingMode === 'event-boost' ? 'override' : 'default',
				runtime_applied: true,
				warning: null
			},
			event_pre_recording_duration_secs: {
				configured_default: 0,
				camera_override: recordingMode === 'event-boost' ? 10 : null,
				effective: recordingMode === 'event-boost' ? 10 : 0,
				source: recordingMode === 'event-boost' ? 'override' : 'default',
				runtime_applied: true,
				warning: null
			},
			event_recording_stream: {
				configured_default: 'main',
				camera_override: null,
				effective: 'main',
				source: 'default',
				runtime_applied: true,
				warning: null
			},
			privacy: null,
			privacy_status: emptyPrivacyStatus
		};
	}

	let currentSnapshot: ConfigurationSnapshot = {
		contract_version: 1,
		configuration_revision: 'rev-8f42c19b7a01',
		defaults: {
			username_configured: true,
			password_configured: true,
			configured_backend: 'retina',
			effective_backend: 'retina',
			configured_transport: 'tcp',
			effective_transport: 'tcp',
			configured_record_generic_motion_events: false,
			effective_record_generic_motion_events: false,
			configured_recording_mode: 'sub',
			effective_recording_mode: 'sub',
			configured_event_recording_duration_secs: 30,
			effective_event_recording_duration_secs: 30,
			configured_event_pre_recording_duration_secs: 0,
			effective_event_pre_recording_duration_secs: 0,
			configured_event_recording_stream: 'main',
			effective_event_recording_stream: 'main',
			privacy: null
		},
		cameras: [
			makeCameraEvidence(
				'front-door',
				'Front Door',
				'192.0.2.41',
				'RLC-811A',
				'reo-proto',
				'tcp',
				'sub',
				'default',
				'default'
			),
			makeCameraEvidence(
				'porch',
				'Porch',
				'192.0.2.42',
				'DS-2CD2143',
				'retina',
				'udp',
				'event-boost',
				'override',
				'override'
			),
			makeCameraEvidence(
				'back-yard',
				'Back Yard',
				'192.0.2.43',
				'RLC-820A',
				'reo-proto',
				'tcp',
				'event-boost',
				'override',
				'default'
			),
			makeCameraEvidence(
				'driveway',
				'Driveway',
				'192.0.2.45',
				'RLC-811A',
				'retina',
				'udp',
				'sub',
				'default',
				'override'
			)
		],
		templates: [perimeterTemplate, indoorTemplate],
		limits: {
			maximum_templates: 64,
			maximum_template_name_bytes: 80,
			maximum_template_description_bytes: 240,
			maximum_plan_targets: 64,
			maximum_import_bytes: 16_384
		},
		domains: [
			{
				domain_id: 'cameras',
				label: 'Camera fleet & templates',
				owner_path: '/cameras',
				capability_id: 'keeppeek.configuration.v1',
				readable: true,
				mutable: true,
				unavailable_reason: null
			},
			{
				domain_id: 'runtime-profiles',
				label: 'Runtime profiles',
				owner_path: '/settings#runtime-profiles',
				capability_id: 'keeppeek.runtime-config.v1',
				readable: true,
				mutable: true,
				unavailable_reason: null
			},
			{
				domain_id: 'storage',
				label: 'Storage & retention',
				owner_path: '/settings#storage',
				capability_id: 'keeppeek.runtime-config.v1',
				readable: true,
				mutable: true,
				unavailable_reason: null
			}
		],
		privacy_default: null
	};

	let lastPlannedTargetIds: string[] = [];

	const mockControlClient = {
		async getConfigurationSnapshot(): Promise<ConfigurationSnapshot> {
			return currentSnapshot;
		},
		async planConfigurationChange(request: ConfigurationPlanRequest): Promise<ConfigurationPlan> {
			const targetIds =
				request.targets?.mode === 'camera-ids'
					? request.targets.camera_ids
					: currentSnapshot.cameras.map((entry) => entry.camera.id);
			lastPlannedTargetIds = targetIds;
			const resolvedTargets = targetIds
				.map((id) => currentSnapshot.cameras.find((entry) => entry.camera.id === id))
				.filter((entry): entry is CameraEffectiveConfiguration => entry !== undefined);
			return {
				plan_id: 'plan-fleet-0042',
				configuration_revision: currentSnapshot.configuration_revision,
				expires_at_ms: Date.parse('2026-08-18T06:47:00Z'),
				authoritative_target_count: resolvedTargets.length,
				targets: resolvedTargets.map((entry) => ({
					camera_id: entry.camera.id,
					display_name: entry.camera.display_name ?? entry.camera.id,
					group_ids: entry.group_ids,
					skipped: false,
					skip_reason: null
				})),
				changes: [
					{
						camera_id: 'front-door',
						field: 'recording_mode',
						old_configured_value: 'inherited (sub)',
						old_effective_value: 'sub',
						new_configured_value: 'event-boost',
						new_effective_value: 'event-boost',
						source: 'template',
						secret: false
					},
					{
						camera_id: 'front-door',
						field: 'event_pre_recording_duration_secs',
						old_configured_value: 'inherited (0s)',
						old_effective_value: '0s',
						new_configured_value: '10s',
						new_effective_value: '10s',
						source: 'template',
						secret: false
					},
					{
						camera_id: 'driveway',
						field: 'transport',
						old_configured_value: 'udp',
						old_effective_value: 'udp',
						new_configured_value: 'tcp',
						new_effective_value: 'tcp',
						source: 'template',
						secret: false
					},
					{
						camera_id: 'driveway',
						field: 'recording_mode',
						old_configured_value: 'inherited (sub)',
						old_effective_value: 'sub',
						new_configured_value: 'event-boost',
						new_effective_value: 'event-boost',
						source: 'template',
						secret: false
					},
					{
						camera_id: 'driveway',
						field: 'password_secret_reference',
						old_configured_value: 'inherited',
						old_effective_value: 'configured',
						new_configured_value: '{secret:CAMERA_PASSWORD}',
						new_effective_value: 'configured ({secret:CAMERA_PASSWORD})',
						source: 'template',
						secret: true
					}
				],
				issues: [],
				impact: 'reconnect-camera',
				valid: true,
				apply_semantics:
					'Writes explicit camera overrides to config.toml atomically, then reconnects affected camera workers without restarting KeepPeek.'
			};
		},
		async applyConfigurationPlan(
			planId: string,
			_expectedRevision: string
		): Promise<ConfigurationApplyResult> {
			currentSnapshot = {
				...currentSnapshot,
				configuration_revision: 'rev-9a71e304d218',
				cameras: currentSnapshot.cameras.map((entry) =>
					lastPlannedTargetIds.includes(entry.camera.id)
						? makeCameraEvidence(
								entry.camera.id,
								entry.camera.display_name ?? entry.camera.id,
								entry.camera.ip,
								entry.camera.model ?? 'RLC-811A',
								'reo-proto',
								'tcp',
								'event-boost',
								'override',
								'override'
							)
						: entry
				)
			};
			return {
				plan_id: planId,
				configuration_committed: true,
				snapshot: currentSnapshot,
				impact: 'reconnect-camera',
				activations: [
					{
						camera_id: 'front-door',
						status: 'applied',
						detail: 'Worker updated to event-boost with 10s pre-roll'
					},
					{
						camera_id: 'driveway',
						status: 'applied',
						detail: 'Worker reconnected on TCP with Perimeter Event Boost'
					}
				]
			};
		},
		async saveConfigurationTemplate(
			_expectedRevision: string,
			template: ConfigurationTemplate
		): Promise<ConfigurationTemplate> {
			return template;
		},
		async duplicateConfigurationTemplate(
			_expectedRevision: string,
			_templateId: string,
			name: string
		): Promise<ConfigurationTemplate> {
			return { ...perimeterTemplate, template_id: 'tpl-copy', name };
		},
		async deleteConfigurationTemplate(): Promise<ConfigurationSnapshot> {
			return currentSnapshot;
		},
		async exportConfigurationTemplates(): Promise<string> {
			return JSON.stringify({ version: 1, templates: currentSnapshot.templates });
		},
		async previewConfigurationTemplateImport() {
			return {
				preview_id: 'preview-1',
				configuration_revision: currentSnapshot.configuration_revision,
				expires_at_ms: Date.parse('2026-08-18T06:47:00Z'),
				templates: currentSnapshot.templates,
				issues: [],
				valid: true
			};
		},
		async applyConfigurationTemplateImport(): Promise<ConfigurationSnapshot> {
			return currentSnapshot;
		}
	};

	setControlClient(mockControlClient as unknown as ControlClient);

	let selectedIds = $state.raw<ReadonlySet<string>>(new Set());
	let configurationOpen = $state(false);
	let managerSelectedIds = $state.raw<string[]>([]);

	function toggleCamera(cameraId: string, selected: boolean): void {
		const next = new Set(selectedIds);
		if (selected) next.add(cameraId);
		else next.delete(cameraId);
		selectedIds = next;
	}

	function openConfiguration(): void {
		// ponytail: Mount FleetConfigurationManager with an empty selection first so it opens on Overview, then hydrate selected IDs on the next tick for Bulk targeting.
		managerSelectedIds = [];
		configurationOpen = true;
		void tick().then(() => {
			managerSelectedIds = [...selectedIds];
		});
	}

	function preventLinks(event: MouseEvent): void {
		if (event.target instanceof Element && event.target.closest('a')) {
			event.preventDefault();
		}
	}
</script>

<main
	data-paper-scenario="cameras.desktop.fleet"
	data-demo-fleet-config
	class="h-[800px] w-[1280px] overflow-hidden bg-ground text-foreground [font-synthesis:none]"
>
	<!-- svelte-ignore a11y_click_events_have_key_events -->
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="flex h-[900px] w-[1440px] origin-top-left overflow-hidden bg-surface"
		style="transform: scale(calc(1280 / 1440));"
		onclick={preventLinks}
	>
		<DesktopPaperRail active="cameras" />

		<section class="flex h-[900px] w-[1376px] shrink-0 flex-col" aria-label="Camera fleet">
			<header
				class="flex h-[52px] shrink-0 items-center justify-between gap-4 border-b border-hairline px-5"
			>
				<div class="flex items-baseline gap-3">
					<h1 class="text-base leading-5 font-semibold">Cameras</h1>
					<p class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-muted">
						6 OF 6 SOURCES
					</p>
				</div>
				<div class="flex items-center gap-2.5">
					<label
						class="flex h-[30px] w-[260px] items-center gap-2 rounded-sm border border-hairline bg-raised px-2.5"
					>
						<SearchIcon class="size-3.5 text-text-faint" />
						<input
							type="search"
							aria-label="Search cameras"
							placeholder="Name, address, model…"
							class="min-w-0 flex-1 bg-transparent text-[13px] outline-none placeholder:text-text-faint"
							readonly
						/>
					</label>
					<Button variant="outline" size="sm" onclick={openConfiguration}>
						<SlidersHorizontalIcon />
						Configuration
					</Button>
				</div>
			</header>

			<div data-camera-fleet-table class="flex min-h-0 flex-1 flex-col px-5">
				<div
					class="grid h-[34px] shrink-0 grid-cols-[32px_20px_270px_140px_230px_150px_140px_120px_152px_80px] items-center border-b border-hairline-strong font-mono text-2xs leading-[14px] tracking-[0.14em] text-text-faint"
				>
					<span></span><span></span><span>CAMERA</span><span>TRANSPORT</span><span>STREAMS</span
					><span>RECORDING</span><span>THROUGHPUT</span><span>GB / DAY</span><span>LAST EVENT</span
					><span></span>
				</div>
				<div
					class="flex h-8 shrink-0 items-center gap-2.5 pt-2 font-mono text-2xs leading-[14px] tracking-[0.14em] text-text-muted"
				>
					<span>CONFIGURED CAMERAS</span><span class="h-px flex-1 bg-hairline"></span><span
						class="text-text-faint">{rows.length}</span
					>
				</div>
				{#each rows as row, index (row.camera.id)}
					<CameraFleetRow
						camera={row.camera}
						presentation={row.presentation}
						selected={selectedIds.has(row.camera.id)}
						tabindex={index === 0 ? 0 : -1}
						onselect={(selected) => toggleCamera(row.camera.id, selected)}
						onfocus={() => {}}
						onkeydown={() => {}}
						paperFrame
					/>
				{/each}
			</div>

			<div
				class="flex h-[68px] shrink-0 items-center justify-between border-t border-hairline px-5"
			>
				<div class="flex items-center gap-3">
					{#if selectedIds.size > 0}
						<div
							class="flex h-10 items-center gap-3 rounded-md border border-hairline-strong bg-raised px-4"
						>
							<span class="text-[13px] font-medium">{selectedIds.size} selected</span>
						</div>
						<Button
							variant="outline"
							size="sm"
							data-demo-action="open-fleet-config"
							onclick={openConfiguration}
						>
							<SlidersHorizontalIcon />
							Manage selected
						</Button>
					{:else}
						<span class="text-xs text-text-muted">Select cameras to configure as a batch.</span>
					{/if}
				</div>
				<span class="font-mono text-2xs leading-[14px] tracking-[0.08em] text-text-faint">
					KEEPPEEK.CONFIGURATION.V1 · SHARED DEFAULTS &amp; TEMPLATES
				</span>
			</div>
		</section>
	</div>

	{#if configurationOpen}
		<FleetConfigurationManager
			selectedCameraIds={managerSelectedIds}
			filteredCameraIds={cameras.map((camera) => camera.id)}
			onclose={() => {
				configurationOpen = false;
			}}
		/>
	{/if}
</main>
