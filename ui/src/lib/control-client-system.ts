import { create } from '@bufbuild/protobuf';
import { timestampDate } from '@bufbuild/protobuf/wkt';
import type {
	Event as ProtoEvent,
	Ok,
	Request,
	HealthProfileSummary,
	LoggingSettingsResult,
	SanitizedRuntimeConfiguration,
	ServerHealthSnapshot
} from './proto/webrtc_pb';
import {
	GetAccessKeySchema,
	GetHealthSchema,
	GetLoggingSettingsSchema,
	GetRuntimeConfigurationSchema,
	HealthCommandSchema,
	LoggingCommandSchema,
	ProbeStorageSchema,
	RestartServerSchema,
	RotateAccessKeySchema,
	RuntimeConfigurationCommandSchema,
	RuntimeStorageConfigurationSchema,
	ServerCommandSchema,
	SetLoggingFilterSchema,
	UpdateRuntimeConfigurationSchema
} from './proto/webrtc_pb';
import type {
	CameraHealth,
	CameraHealthDimensions,
	CameraHealthReason,
	CameraHealthState,
	LoggingSettings,
	ProfileSummary,
	RecordingEvent,
	SanitizedConfig,
	ServerHealthResponse,
	SettingsConfigUpdate,
	SettingsConfigUpdateResponse,
	StreamHealth,
	StreamHealthDimensions
} from './types';
import type { StorageWriteProbe } from './first-run';

type SendRequest = (command: Request['command']) => Promise<Ok['result']>;
type MapRecordingEvent = (event: ProtoEvent) => RecordingEvent;

export class SystemControlClient {
	constructor(
		private readonly sendRequest: SendRequest,
		private readonly mapRecordingEvent: MapRecordingEvent
	) {}

	async getHealth(signal?: AbortSignal): Promise<ServerHealthResponse> {
		signal?.throwIfAborted();
		const command = create(HealthCommandSchema, {
			action: { case: 'get', value: create(GetHealthSchema) }
		});
		const result = await this.sendRequest({ case: 'healthCommand', value: command });
		signal?.throwIfAborted();
		if (result.case !== 'healthResult') {
			throw new Error('Server returned an unexpected health response.');
		}
		return serverHealth(result.value, this.mapRecordingEvent);
	}

	async getLoggingSettings(): Promise<LoggingSettings> {
		const command = create(LoggingCommandSchema, {
			action: { case: 'getSettings', value: create(GetLoggingSettingsSchema) }
		});
		return this.loggingRequest(command);
	}

	async setLoggingFilter(filter: string): Promise<LoggingSettings> {
		const command = create(LoggingCommandSchema, {
			action: {
				case: 'setFilter',
				value: create(SetLoggingFilterSchema, { filter })
			}
		});
		return this.loggingRequest(command);
	}

	async restartServer(): Promise<void> {
		const command = create(ServerCommandSchema, {
			action: { case: 'restart', value: create(RestartServerSchema) }
		});
		const result = await this.sendRequest({ case: 'serverCommand', value: command });
		if (result.case !== 'restartResult' || !result.value.restarting) {
			throw new Error('Server did not acknowledge the restart request.');
		}
	}

	async revealAccessKey(): Promise<string> {
		const command = create(ServerCommandSchema, {
			action: { case: 'getAccessKey', value: create(GetAccessKeySchema) }
		});
		const result = await this.sendRequest({ case: 'serverCommand', value: command });
		if (result.case !== 'accessKeyResult' || result.value.rotated || !result.value.accessKey) {
			throw new Error('Server returned an unexpected access key response.');
		}
		return result.value.accessKey;
	}

	async rotateAccessKey(): Promise<string> {
		const command = create(ServerCommandSchema, {
			action: { case: 'rotateAccessKey', value: create(RotateAccessKeySchema) }
		});
		const result = await this.sendRequest({ case: 'serverCommand', value: command });
		if (result.case !== 'accessKeyResult' || !result.value.rotated || !result.value.accessKey) {
			throw new Error('Server did not return the rotated access key.');
		}
		return result.value.accessKey;
	}

	async updateRuntimeConfiguration(
		update: SettingsConfigUpdate
	): Promise<SettingsConfigUpdateResponse> {
		const storage = create(RuntimeStorageConfigurationSchema, {
			mediumTermPath: update.storage.medium_term_path,
			longTermPath: update.storage.long_term_path,
			recordingCatalogPath: update.storage.recording_catalog_path,
			eventThumbnailPath: update.storage.event_thumbnail_path,
			eventThumbnailMaxMb: BigInt(update.storage.event_thumbnail_max_mb),
			shortTermSecs: BigInt(update.storage.short_term_secs),
			mediumTermSecs: BigInt(update.storage.medium_term_secs),
			flushIntervalSecs: BigInt(update.storage.flush_interval_secs),
			writeBufferBytes: BigInt(update.storage.write_buffer_bytes),
			longTermMaxGb: BigInt(update.storage.long_term_max_gb),
			minimumFreeGb: BigInt(update.storage.minimum_free_gb ?? 0),
			maximumUsedPercent: update.storage.maximum_used_percent ?? 0,
			warningFreeGb: BigInt(update.storage.warning_free_gb ?? 0),
			criticalFreeGb: BigInt(update.storage.critical_free_gb ?? 0),
			cleanupHysteresisGb: BigInt(update.storage.cleanup_hysteresis_gb ?? 0)
		});
		const command = create(RuntimeConfigurationCommandSchema, {
			action: {
				case: 'update',
				value: create(UpdateRuntimeConfigurationSchema, {
					host: update.host,
					port: update.port,
					storage,
					moveExistingRecordings: update.move_existing_recordings,
					expectedConfigurationRevision: update.expected_configuration_revision ?? ''
				})
			}
		});
		const result = await this.sendRequest({ case: 'runtimeConfigurationCommand', value: command });
		if (result.case !== 'runtimeConfigurationResult' || !result.value.config) {
			throw new Error('Server returned an unexpected runtime configuration response.');
		}
		return {
			config: runtimeConfiguration(result.value.config),
			restart_required: result.value.restartRequired
		};
	}

	async getRuntimeConfiguration(): Promise<SanitizedConfig> {
		const command = create(RuntimeConfigurationCommandSchema, {
			action: { case: 'get', value: create(GetRuntimeConfigurationSchema) }
		});
		const result = await this.sendRequest({ case: 'runtimeConfigurationCommand', value: command });
		if (result.case !== 'runtimeConfigurationResult' || !result.value.config) {
			throw new Error('Server returned an unexpected runtime configuration response.');
		}
		return runtimeConfiguration(result.value.config);
	}

	async probeStorage(path: string): Promise<StorageWriteProbe> {
		const command = create(RuntimeConfigurationCommandSchema, {
			action: { case: 'probeStorage', value: create(ProbeStorageSchema, { path }) }
		});
		const result = await this.sendRequest({ case: 'runtimeConfigurationCommand', value: command });
		if (result.case !== 'storageWriteProbeResult') {
			throw new Error('Server returned an unexpected storage write probe response.');
		}
		return { writable: result.value.writable, detail: result.value.detail };
	}

	private async loggingRequest(
		command: ReturnType<typeof create<typeof LoggingCommandSchema>>
	): Promise<LoggingSettings> {
		const result = await this.sendRequest({ case: 'loggingCommand', value: command });
		if (result.case !== 'loggingSettingsResult') {
			throw new Error('Server returned an unexpected logging response.');
		}
		return loggingSettings(result.value);
	}
}

export function loggingSettings(result: LoggingSettingsResult): LoggingSettings {
	const buffer = result.buffer;
	if (!buffer) throw new Error('Server returned logging settings without buffer evidence.');
	return {
		active_filter: result.activeFilter,
		default_filter: result.defaultFilter,
		filter_error: result.filterError ?? null,
		version: result.version,
		buffer: {
			entry_count: numeric(buffer.entryCount),
			byte_count: numeric(buffer.byteCount),
			evicted_entries: numeric(buffer.evictedEntries),
			max_entries: numeric(buffer.maxEntries),
			max_bytes: numeric(buffer.maxBytes),
			active_streams: numeric(buffer.activeStreams),
			max_streams: numeric(buffer.maxStreams)
		}
	};
}

export function numeric(value: bigint): number {
	return Number(value > BigInt(Number.MAX_SAFE_INTEGER) ? BigInt(Number.MAX_SAFE_INTEGER) : value);
}

export function runtimeConfiguration(config: SanitizedRuntimeConfiguration): SanitizedConfig {
	if (!config.storage || !config.recordingEstimate) {
		throw new Error('Server returned incomplete runtime configuration evidence.');
	}
	return {
		host: config.host,
		port: config.port,
		configuration_revision: config.configurationRevision,
		storage: {
			medium_term_path: config.storage.mediumTermPath,
			long_term_path: config.storage.longTermPath,
			recording_catalog_path: config.storage.recordingCatalogPath,
			event_thumbnail_path: config.storage.eventThumbnailPath,
			event_thumbnail_max_mb: numeric(config.storage.eventThumbnailMaxMb),
			short_term_secs: numeric(config.storage.shortTermSecs),
			medium_term_secs: numeric(config.storage.mediumTermSecs),
			flush_interval_secs: numeric(config.storage.flushIntervalSecs),
			write_buffer_bytes: numeric(config.storage.writeBufferBytes),
			long_term_max_gb: numeric(config.storage.longTermMaxGb),
			minimum_free_gb:
				config.storage.minimumFreeGb === undefined ? 0 : numeric(config.storage.minimumFreeGb),
			maximum_used_percent:
				config.storage.maximumUsedPercent === undefined || config.storage.maximumUsedPercent === 0
					? null
					: config.storage.maximumUsedPercent,
			warning_free_gb:
				config.storage.warningFreeGb === undefined ? 0 : numeric(config.storage.warningFreeGb),
			critical_free_gb:
				config.storage.criticalFreeGb === undefined ? 0 : numeric(config.storage.criticalFreeGb),
			cleanup_hysteresis_gb:
				config.storage.cleanupHysteresisGb === undefined
					? 0
					: numeric(config.storage.cleanupHysteresisGb)
		},
		camera_count: numeric(config.cameraCount),
		recording_estimate: {
			estimated_bitrate_bps: numeric(config.recordingEstimate.estimatedBitrateBps),
			bytes_per_day: numeric(config.recordingEstimate.bytesPerDay),
			known_streams: numeric(config.recordingEstimate.knownStreams),
			unknown_streams: numeric(config.recordingEstimate.unknownStreams),
			estimated_retention_days: config.recordingEstimate.estimatedRetentionDays ?? null
		}
	};
}

export function serverHealth(
	health: ServerHealthSnapshot,
	mapRecordingEvent: MapRecordingEvent
): ServerHealthResponse {
	if (health.healthContractVersion < 1) {
		throw new Error(`Server returned unsupported health contract ${health.healthContractVersion}.`);
	}
	const { totals, system, storage, webrtc } = health;
	if (!totals || !system || !storage || !webrtc) {
		throw new Error('Server returned incomplete health evidence.');
	}
	const { process, memory, load } = system;
	const demand = storage.demand;
	const safety = storage.safety;
	if (!process || !memory || !load || !demand) {
		throw new Error('Server returned incomplete health evidence.');
	}
	return {
		status: health.status === 'healthy' ? 'healthy' : 'degraded',
		health_contract_version: health.healthContractVersion,
		generated_at_ms: numeric(health.generatedAtMs),
		uptime_seconds: numeric(health.uptimeSeconds),
		version: health.version,
		totals: {
			configured_cameras: numeric(totals.configuredCameras),
			connected_cameras: numeric(totals.connectedCameras),
			fresh_cameras: numeric(totals.freshCameras),
			decodable_cameras: numeric(totals.decodableCameras),
			recording_requested_cameras: numeric(totals.recordingRequestedCameras),
			recording_cameras: numeric(totals.recordingCameras),
			unknown_cameras: numeric(totals.unknownCameras),
			configured_video_streams: numeric(totals.configuredVideoStreams),
			connected_video_streams: numeric(totals.connectedVideoStreams),
			fresh_video_streams: numeric(totals.freshVideoStreams),
			decodable_video_streams: numeric(totals.decodableVideoStreams),
			recording_requested_video_streams: numeric(totals.recordingRequestedVideoStreams),
			recording_video_streams: numeric(totals.recordingVideoStreams),
			ingress_fps: totals.ingressFps,
			ingress_bitrate_bps: numeric(totals.ingressBitrateBps),
			frames: numeric(totals.frames),
			keyframes: numeric(totals.keyframes),
			drops: numeric(totals.drops),
			errors: numeric(totals.errors),
			reconnects: numeric(totals.reconnects)
		},
		system: {
			host_name: system.hostName ?? null,
			os_name: system.osName ?? null,
			os_version: system.osVersion ?? null,
			kernel_version: system.kernelVersion ?? null,
			architecture: system.architecture,
			system_uptime_seconds: numeric(system.systemUptimeSeconds),
			boot_time_seconds: numeric(system.bootTimeSeconds),
			logical_cores: numeric(system.logicalCores),
			physical_cores: optionalNumber(system.physicalCores),
			cpu_brand: system.cpuBrand ?? null,
			system_cpu_percent: system.systemCpuPercent,
			process: {
				pid: process.pid,
				name: process.name ?? null,
				executable: process.executable ?? null,
				working_directory: process.workingDirectory ?? null,
				cpu_percent: process.cpuPercent ?? null,
				cpu_capacity_percent: process.cpuCapacityPercent ?? null,
				cpu_core_equivalents: process.cpuCoreEquivalents ?? null,
				resident_memory_bytes: optionalNumber(process.residentMemoryBytes),
				memory_capacity_percent: process.memoryCapacityPercent ?? null,
				virtual_memory_bytes: optionalNumber(process.virtualMemoryBytes),
				started_at_seconds: optionalNumber(process.startedAtSeconds),
				uptime_seconds: optionalNumber(process.uptimeSeconds),
				tasks: optionalNumber(process.tasks),
				read_bytes_per_second: optionalNumber(process.readBytesPerSecond),
				write_bytes_per_second: optionalNumber(process.writeBytesPerSecond),
				total_read_bytes: optionalNumber(process.totalReadBytes),
				total_written_bytes: optionalNumber(process.totalWrittenBytes)
			},
			memory: {
				total_bytes: numeric(memory.totalBytes),
				used_bytes: numeric(memory.usedBytes),
				available_bytes: numeric(memory.availableBytes),
				total_swap_bytes: numeric(memory.totalSwapBytes),
				used_swap_bytes: numeric(memory.usedSwapBytes)
			},
			load: {
				one_minute: load.oneMinute,
				five_minutes: load.fiveMinutes,
				fifteen_minutes: load.fifteenMinutes
			},
			cpus: system.cpus.map((cpu) => ({
				name: cpu.name,
				usage_percent: cpu.usagePercent,
				frequency_mhz: numeric(cpu.frequencyMhz)
			})),
			network_egress_bps: numeric(system.networkEgressBps),
			networks: system.networks.map((network) => ({
				name: network.name,
				received_bytes_per_second: numeric(network.receivedBytesPerSecond),
				transmitted_bytes_per_second: numeric(network.transmittedBytesPerSecond),
				received_packets_per_second: numeric(network.receivedPacketsPerSecond),
				transmitted_packets_per_second: numeric(network.transmittedPacketsPerSecond),
				receive_errors: numeric(network.receiveErrors),
				transmit_errors: numeric(network.transmitErrors),
				total_received_bytes: numeric(network.totalReceivedBytes),
				total_transmitted_bytes: numeric(network.totalTransmittedBytes)
			})),
			disks: system.disks.map((disk) => ({
				name: disk.name,
				kind: disk.kind,
				file_system: disk.fileSystem,
				mount_point: disk.mountPoint,
				total_bytes: numeric(disk.totalBytes),
				available_bytes: numeric(disk.availableBytes),
				used_bytes: numeric(disk.usedBytes),
				removable: disk.removable,
				stores_recordings: disk.storesRecordings
			})),
			temperatures: system.temperatures.map((temperature) => ({
				label: temperature.label,
				current_celsius: temperature.currentCelsius ?? null,
				max_celsius: temperature.maxCelsius ?? null,
				critical_celsius: temperature.criticalCelsius ?? null
			}))
		},
		storage: {
			medium_term_path: storage.mediumTermPath,
			long_term_path: storage.longTermPath,
			paths_are_same: storage.pathsAreSame,
			short_term_seconds: numeric(storage.shortTermSeconds),
			medium_term_seconds: numeric(storage.mediumTermSeconds),
			flush_interval_seconds: numeric(storage.flushIntervalSeconds),
			write_buffer_bytes: numeric(storage.writeBufferBytes),
			long_term_max_bytes: numeric(storage.longTermMaxBytes),
			minimum_free_bytes: numeric(storage.minimumFreeBytes),
			maximum_used_percent: storage.maximumUsedPercent ?? null,
			warning_free_bytes: numeric(storage.warningFreeBytes),
			critical_free_bytes: numeric(storage.criticalFreeBytes),
			cleanup_hysteresis_bytes: numeric(storage.cleanupHysteresisBytes),
			catalog_bytes: optionalNumber(storage.catalogBytes),
			catalog: storage.catalog
				? {
						recording_files: numeric(storage.catalog.recordingFiles),
						finalized_files: numeric(storage.catalog.finalizedFiles),
						active_files: numeric(storage.catalog.activeFiles),
						protected_files: numeric(storage.catalog.protectedFiles),
						recording_bytes: numeric(storage.catalog.recordingBytes),
						fragments: numeric(storage.catalog.fragments),
						fragment_bytes: numeric(storage.catalog.fragmentBytes),
						events: numeric(storage.catalog.events),
						open_events: numeric(storage.catalog.openEvents),
						event_thumbnails: numeric(storage.catalog.eventThumbnails),
						oldest_recording_at_ms: optionalNumber(storage.catalog.oldestRecordingAtMs),
						newest_recording_at_ms: optionalNumber(storage.catalog.newestRecordingAtMs)
					}
				: null,
			safety: safety
				? {
						pressure: safety.pressure as 'normal' | 'warning' | 'critical',
						recording_state: safety.recordingState as 'active' | 'degraded' | 'paused',
						total_bytes: optionalNumber(safety.totalBytes),
						available_bytes: optionalNumber(safety.availableBytes),
						keeppeek_bytes: optionalNumber(safety.keeppeekBytes),
						effective_limit_bytes: optionalNumber(safety.effectiveLimitBytes),
						cleanup_target_bytes: optionalNumber(safety.cleanupTargetBytes),
						warning_free_bytes: numeric(safety.warningFreeBytes),
						critical_free_bytes: numeric(safety.criticalFreeBytes),
						recovery_free_bytes: numeric(safety.recoveryFreeBytes),
						last_evaluation_at_ms: optionalNumber(safety.lastEvaluationAtMs),
						last_evaluation_trigger:
							(safety.lastEvaluationTrigger as
								'startup' | 'segment_finalized' | 'periodic' | undefined) ?? null,
						cleanup_running: safety.cleanupRunning,
						last_cleanup_started_at_ms: optionalNumber(safety.lastCleanupStartedAtMs),
						last_cleanup_ended_at_ms: optionalNumber(safety.lastCleanupEndedAtMs),
						last_cleanup_files_removed: numeric(safety.lastCleanupFilesRemoved),
						last_cleanup_bytes_removed: numeric(safety.lastCleanupBytesRemoved),
						last_cleanup_reason:
							(safety.lastCleanupReason as
								| 'archive_cap'
								| 'filesystem_headroom'
								| 'combined'
								| 'reconciliation'
								| undefined) ?? null,
						last_failure_at_ms: optionalNumber(safety.lastFailureAtMs),
						last_failure: safety.lastFailure ?? null,
						last_recovered_at_ms: optionalNumber(safety.lastRecoveredAtMs)
					}
				: null,
			demand: {
				active_streams: numeric(demand.activeStreams),
				total_viewers: numeric(demand.totalViewers),
				leased_streams: numeric(demand.leasedStreams),
				streams: demand.streams.map((stream) => ({
					stream_id: stream.streamId,
					viewers: numeric(stream.viewers),
					lease_remaining_ms: optionalNumber(stream.leaseRemainingMs)
				}))
			}
		},
		webrtc: {
			active_sessions: numeric(webrtc.activeSessions),
			adaptive_sessions: numeric(webrtc.adaptiveSessions),
			browser_sessions: numeric(webrtc.multiTrackSessions),
			browser_tracks: numeric(webrtc.multiTracks),
			fixed_sessions: numeric(webrtc.fixedSessions),
			active_main: numeric(webrtc.activeMain),
			active_sub: numeric(webrtc.activeSub),
			requested_auto: numeric(webrtc.requestedAuto),
			requested_high: numeric(webrtc.requestedHigh),
			requested_low: numeric(webrtc.requestedLow),
			estimated_bitrate_min_bps: optionalNumber(webrtc.estimatedBitrateMinBps),
			estimated_bitrate_avg_bps: optionalNumber(webrtc.estimatedBitrateAvgBps),
			estimated_bitrate_max_bps: optionalNumber(webrtc.estimatedBitrateMaxBps),
			source_bitrate_bps: numeric(webrtc.sourceBitrateBps),
			published_frames: numeric(webrtc.publishedFrames),
			published_bytes: numeric(webrtc.publishedBytes),
			delivered_frames: numeric(webrtc.deliveredFrames),
			written_frames: numeric(webrtc.writtenFrames),
			queue_capacity: numeric(webrtc.queueCapacity),
			queued_frames: numeric(webrtc.queuedFrames),
			queue_depth_max: numeric(webrtc.queueDepthMax),
			queue_high_water: numeric(webrtc.queueHighWater),
			queue_drops: numeric(webrtc.queueDrops),
			queue_discarded_frames: numeric(webrtc.queueDiscardedFrames),
			queue_recovery_drops: numeric(webrtc.queueRecoveryDrops),
			session_queues: webrtc.sessionQueues.map((queue) => ({
				session_id: numeric(queue.sessionId),
				track_id: queue.trackId ?? null,
				camera_ip: queue.cameraIp,
				stream: healthStream(queue.stream),
				depth: numeric(queue.depth),
				high_water: numeric(queue.highWater),
				written_frames: numeric(queue.writtenFrames),
				full_drops: numeric(queue.fullDrops),
				discarded_frames: numeric(queue.discardedFrames),
				recovery_drops: numeric(queue.recoveryDrops)
			})),
			sources: webrtc.sources.map((source) => ({
				camera_ip: source.cameraIp,
				stream: healthStream(source.stream),
				subscribers: numeric(source.subscribers),
				bitrate_bps: optionalNumber(source.bitrateBps),
				has_keyframe: source.hasKeyframe,
				keyframe_age_ms: optionalNumber(source.keyframeAgeMs)
			}))
		},
		cameras: health.cameras.map(cameraHealth),
		issues: health.issues.map((issue) => ({
			severity: issue.severity as 'critical' | 'warning' | 'info',
			scope: issue.scope,
			message: issue.message,
			operational_event_id: issue.operationalEventId ?? null,
			timeline_start_ms: issue.timelineStart ? timestampDate(issue.timelineStart).getTime() : null,
			timeline_end_ms: issue.timelineEnd ? timestampDate(issue.timelineEnd).getTime() : null
		})),
		operational_events: health.operationalEvents.map(mapRecordingEvent)
	};
}

function cameraHealth(camera: ServerHealthSnapshot['cameras'][number]): CameraHealth {
	const state = canonicalCameraHealthState(camera.state);
	const reason = canonicalCameraHealthReason(camera.reason);
	return {
		id: camera.id,
		ip: camera.ip,
		name: camera.name,
		manufacturer: camera.manufacturer ?? null,
		model: camera.model ?? null,
		firmware_version: camera.firmwareVersion ?? null,
		backend: camera.backend,
		transport: camera.transport,
		state,
		reason,
		reason_codes:
			camera.reasonCodes.length > 0
				? camera.reasonCodes.map(canonicalCameraHealthReason)
				: [reason],
		detail: camera.detail || camera.lastError || fallbackHealthDetail(state),
		dimensions: camera.dimensions ? cameraHealthDimensions(camera.dimensions) : null,
		lifecycle: camera.lifecycle ?? null,
		last_error: camera.lastError ?? null,
		configured_profiles: camera.configuredProfiles.map(healthProfile),
		streams: camera.streams.map(streamHealth)
	};
}

function cameraHealthDimensions(
	dimensions: NonNullable<ServerHealthSnapshot['cameras'][number]['dimensions']>
): CameraHealthDimensions {
	return {
		configured: dimensions.configured,
		expected: dimensions.expected,
		configured_video_streams: numeric(dimensions.configuredVideoStreams),
		connected_video_streams: optionalNumber(dimensions.connectedVideoStreams),
		reporting_video_streams: numeric(dimensions.reportingVideoStreams),
		fresh_video_streams: numeric(dimensions.freshVideoStreams),
		decodable_video_streams: numeric(dimensions.decodableVideoStreams),
		configured_video_stream_ids: dimensions.configuredVideoStreamIds,
		connected_video_stream_ids: dimensions.connectedVideoStreamIdsKnown
			? dimensions.connectedVideoStreamIds
			: null,
		reporting_video_stream_ids: dimensions.reportingVideoStreamIds,
		fresh_video_stream_ids: dimensions.freshVideoStreamIds,
		decodable_video_stream_ids: dimensions.decodableVideoStreamIds,
		transport_connected: dimensions.transportConnected ?? null,
		latest_report_at_ms: optionalNumber(dimensions.latestReportAtMs),
		report_age_ms: optionalNumber(dimensions.reportAgeMs),
		frames_fresh: dimensions.framesFresh ?? null,
		decodable: dimensions.decodable ?? null,
		recent_reconnects: numeric(dimensions.recentReconnects),
		recent_drops: numeric(dimensions.recentDrops),
		recent_errors: numeric(dimensions.recentErrors),
		recording_requested: dimensions.recordingRequested,
		recording_video_streams: numeric(dimensions.recordingVideoStreams),
		recording_streams_progressing: numeric(dimensions.recordingStreamsProgressing),
		recording_video_stream_ids: dimensions.recordingVideoStreamIds,
		recording_progressing_stream_ids: dimensions.recordingProgressingStreamIds,
		recording_progressing: dimensions.recordingProgressing ?? null,
		recording_progress_age_ms: optionalNumber(dimensions.recordingProgressAgeMs),
		session_duration_ms: optionalNumber(dimensions.sessionDurationMs),
		recorded_main_duration_ms: numeric(dimensions.recordedMainDurationMs),
		recorded_sub_duration_ms: numeric(dimensions.recordedSubDurationMs),
		recorded_total_duration_ms: numeric(dimensions.recordedTotalDurationMs),
		battery_configured: dimensions.batteryConfigured,
		battery_registered: dimensions.batteryRegistered ?? null,
		battery_last_seen_age_ms: optionalNumber(dimensions.batteryLastSeenAgeMs),
		battery_wake_pending_age_ms: optionalNumber(dimensions.batteryWakePendingAgeMs),
		battery_sleeping: dimensions.batterySleeping ?? null
	};
}

export function healthProfile(profile: HealthProfileSummary): ProfileSummary {
	return {
		name: profile.name,
		stream: healthStream(profile.stream),
		encoding: profile.encoding ?? null,
		resolution: profile.resolution ?? null,
		framerate: profile.framerate ?? null,
		bitrate_kbps: profile.bitrateKbps ?? null,
		gop: profile.gop ?? null,
		h264_profile: profile.h264Profile ?? null,
		audio: profile.audio
			? {
					encoding: profile.audio.encoding,
					sample_rate: profile.audio.sampleRate ?? null,
					bitrate_kbps: profile.audio.bitrateKbps ?? null
				}
			: null
	};
}

function streamHealth(
	stream: ServerHealthSnapshot['cameras'][number]['streams'][number]
): StreamHealth {
	const state = canonicalCameraHealthState(stream.state);
	const reason = canonicalCameraHealthReason(stream.reason);
	return {
		type: stream.type,
		codec: stream.codec,
		resolution: stream.resolution,
		fps: stream.fps,
		expected_fps: stream.expectedFps,
		kf_fps: stream.kfFps,
		kbps: stream.kbps,
		max_frame_kb: stream.maxFrameKb,
		gap_min_ms: stream.gapMinMs,
		gap_avg_ms: stream.gapAvgMs,
		gap_max_ms: stream.gapMaxMs,
		jitter_samples: optionalUndefinedNumber(stream.jitterSamples),
		jitter_p50_ms: stream.jitterP50Ms,
		jitter_p99_ms: stream.jitterP99Ms,
		frames: optionalUndefinedNumber(stream.frames),
		bytes: optionalUndefinedNumber(stream.bytes),
		keyframes: optionalUndefinedNumber(stream.keyframes),
		reconnects: optionalUndefinedNumber(stream.reconnects),
		drops: optionalUndefinedNumber(stream.drops),
		errors: optionalUndefinedNumber(stream.errors),
		updated_at_ms: numeric(stream.updatedAtMs),
		report_age_ms: numeric(stream.reportAgeMs),
		frame_updated_at_ms: optionalNumber(stream.frameUpdatedAtMs),
		frame_age_ms: optionalNumber(stream.frameAgeMs),
		keyframe_updated_at_ms: optionalNumber(stream.keyframeUpdatedAtMs),
		keyframe_age_ms: optionalNumber(stream.keyframeAgeMs),
		recent_reconnects: numeric(stream.recentReconnects),
		recent_drops: numeric(stream.recentDrops),
		recent_errors: numeric(stream.recentErrors),
		state,
		reason,
		reason_codes:
			stream.reasonCodes.length > 0
				? stream.reasonCodes.map(canonicalCameraHealthReason)
				: [reason],
		detail: stream.detail || fallbackHealthDetail(state),
		dimensions: stream.dimensions ? streamHealthDimensions(stream.dimensions) : null
	};
}

function streamHealthDimensions(
	dimensions: NonNullable<ServerHealthSnapshot['cameras'][number]['streams'][number]['dimensions']>
): StreamHealthDimensions {
	return {
		expected: dimensions.expected,
		transport_connected: dimensions.transportConnected ?? null,
		report_fresh: dimensions.reportFresh,
		report_freshness_threshold_ms: numeric(dimensions.reportFreshnessThresholdMs),
		frames_fresh: dimensions.framesFresh,
		frame_freshness_threshold_ms: numeric(dimensions.frameFreshnessThresholdMs),
		decodable: dimensions.decodable,
		keyframe_freshness_threshold_ms: numeric(dimensions.keyframeFreshnessThresholdMs),
		recent_reconnects: numeric(dimensions.recentReconnects),
		recent_drops: numeric(dimensions.recentDrops),
		recent_errors: numeric(dimensions.recentErrors),
		recording_requested: dimensions.recordingRequested,
		recording_progressing: dimensions.recordingProgressing ?? null,
		recording_progress_age_ms: optionalNumber(dimensions.recordingProgressAgeMs),
		session_duration_ms: numeric(dimensions.sessionDurationMs),
		recorded_duration_ms: numeric(dimensions.recordedDurationMs)
	};
}

const cameraHealthStates: ReadonlySet<string> = new Set<CameraHealthState>([
	'starting',
	'healthy',
	'degraded',
	'stale',
	'reconnecting',
	'offline',
	'stopped',
	'unknown'
]);

const cameraHealthReasons: ReadonlySet<string> = new Set<CameraHealthReason>([
	'healthy',
	'starting',
	'not_expected',
	'battery_sleeping',
	'evidence_unavailable',
	'transport_disconnected',
	'transport_reconnecting',
	'transport_partially_connected',
	'no_stream_report',
	'stream_report_stale',
	'frames_not_arriving',
	'frames_below_expected',
	'keyframes_missing',
	'ingress_reconnects',
	'ingress_drops',
	'ingress_errors',
	'recording_not_progressing',
	'unknown'
]);

function canonicalCameraHealthState(value: string): CameraHealthState {
	return cameraHealthStates.has(value) ? (value as CameraHealthState) : 'unknown';
}

function canonicalCameraHealthReason(value: string): CameraHealthReason {
	return cameraHealthReasons.has(value) ? (value as CameraHealthReason) : 'unknown';
}

function fallbackHealthDetail(state: CameraHealthState): string {
	if (state === 'healthy') return 'Camera evidence is current';
	if (state === 'starting') return 'Waiting for initial camera evidence';
	if (state === 'stale') return 'Camera media evidence is stale';
	if (state === 'reconnecting') return 'Camera transport is reconnecting';
	if (state === 'offline') return 'Camera transport is offline';
	if (state === 'stopped') return 'Camera media is not expected';
	if (state === 'degraded') return 'Camera health is degraded';
	return 'Camera health evidence is unavailable';
}

function healthStream(value: string): 'main' | 'sub' {
	if (value === 'main' || value === 'sub') return value;
	throw new Error(`Server returned an unexpected health stream '${value}'.`);
}

function optionalNumber(value: bigint | undefined): number | null {
	return value === undefined ? null : numeric(value);
}

function optionalUndefinedNumber(value: bigint | undefined): number | undefined {
	return value === undefined ? undefined : numeric(value);
}
