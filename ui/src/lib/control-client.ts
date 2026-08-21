import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import { durationFromMs, timestampDate, timestampFromDate } from '@bufbuild/protobuf/wkt';
import { createSession, deleteSession } from './api';
import {
	CameraControlCommandSchema,
	CameraConfigurationCommandSchema,
	CameraBackend as ProtoCameraBackend,
	CameraTransport as ProtoCameraTransport,
	CancelStoredMediaTimelineQuerySchema,
	ControlEnvelopeSchema,
	CloseStoredMediaSchema,
	DataChannelKind,
	DiscoverCamerasSchema,
	DownloadExportSchema,
	EventOrigin,
	ExportCommandSchema,
	ExportJobStatus,
	GetExportJobSchema,
	GetCameraConfigurationsSchema,
	GetHealthSchema,
	GetLoggingSettingsSchema,
	GetMotionDetectionSchema,
	GetRuntimeConfigurationSchema,
	ListExportJobsSchema,
	LoggingCommandSchema,
	HealthCommandSchema,
	MessageSchema,
	OpenStoredMediaSchema,
	RefillStoredMediaSchema,
	RetryExportJobSchema,
	OptionalStringUpdateSchema,
	OptionalUint32UpdateSchema,
	PtzCommandSchema,
	PtzContinuousSchema,
	PtzPresetGotoSchema,
	PtzPresetListSchema,
	PtzStopSchema,
	RemoveCameraConfigurationSchema,
	RuntimeConfigurationCommandSchema,
	RuntimeStorageConfigurationSchema,
	RequestSchema,
	RestartServerSchema,
	ServerCommandSchema,
	SetCameraManufacturerSchema,
	SetLoggingFilterSchema,
	SetMotionDetectionSchema,
	SetStoredMediaPlaybackSchema,
	StoredMediaCommandSchema,
	StoredMediaEventQuerySchema,
	StoredMediaMode,
	StoredMediaStatus,
	CancelExportJobSchema,
	CreateExportJobSchema,
	QueryStoredMediaTimelineSchema,
	UpdateCameraConfigurationSchema,
	UpdateRuntimeConfigurationSchema,
	type LoggingSettingsResult,
	type SanitizedRuntimeConfiguration,
	type ServerHealthSnapshot,
	type Event as ProtoEvent,
	type ExportJob as ProtoExportJob,
	type ServerCapabilities,
	type StoredMediaFragment,
	type StoredMediaInitialization,
	type StoredMediaState,
	type MotionDetectionResult,
	type Request,
	type Response as ControlResponse
} from './proto/webrtc_pb';
import type {
	MotionDetection,
	RecordingEvent,
	RecordingEventsResponse,
	RecordingSegment,
	RecordingsResponse
} from './types';
import type { LoggingSettings } from './types';
import type { DiscoveredCameraSettings } from './types';
import type {
	CameraBackend,
	CameraDetailsResponse,
	CameraListItem,
	CameraSettings,
	CameraSettingsUpdate,
	CameraSettingsUpdateResponse,
	CameraTransport
} from './types';
import type { SanitizedConfig, SettingsConfigUpdate, SettingsConfigUpdateResponse } from './types';
import type { CameraHealth, ProfileSummary, ServerHealthResponse, StreamHealth } from './types';

const controlTimeoutMs = 10_000;

type PendingRequest = {
	resolve: (response: ControlResponse) => void;
	reject: (error: Error) => void;
	timeout: ReturnType<typeof setTimeout>;
};

type CapabilityListener = (capabilityIds: readonly string[]) => void;

export type PtzPreset = { id: number; name: string };

export type MediaExportJobStatus =
	'running' | 'partial' | 'ready' | 'failed' | 'cancelled' | 'expired';

export type MediaExportJob = {
	id: string;
	sourceId: string;
	streamId: 'main' | 'sub';
	requestedStartMs: number;
	requestedEndMs: number;
	alignedStartMs: number | null;
	status: MediaExportJobStatus;
	progress: number;
	bytesWritten: number;
	estimatedBytes: number | null;
	fileName: string | null;
	sha256: string | null;
	expiresAtMs: number | null;
	missingRanges: Array<{ startMs: number; endMs: number }>;
	error: string | null;
	retryable: boolean;
	burnInTimestamp: boolean;
};

export type MediaExportDownload = {
	job: MediaExportJob;
	blob: Blob;
};

type StoredTimelineRange = {
	sourceId: string;
	streamId: string;
	startMs: number;
	endMs: number;
};

type StoredTimelineResult = {
	ranges: StoredTimelineRange[];
	events: RecordingEvent[];
};

type TimelinePending = {
	ranges: StoredTimelineRange[];
	events: ProtoEvent[];
	pages: Set<number>;
	attachments: Map<string, ChunkAccumulator>;
	resolve: (result: StoredTimelineResult) => void;
	reject: (error: Error) => void;
	timeout: ReturnType<typeof setTimeout>;
};

type ExportDownloadPending = {
	job: MediaExportJob | null;
	expectedChunks: number | null;
	chunks: Array<Uint8Array | undefined>;
	completing: boolean;
	resolve: (download: MediaExportDownload) => void;
	reject: (error: Error) => void;
	timeout: ReturnType<typeof setTimeout>;
};

type ChunkAccumulator = {
	chunkCount: number;
	chunks: Array<Uint8Array | undefined>;
	contentType: string;
};

type CapabilityWaiter = {
	resolve: (capabilities: ServerCapabilities) => void;
	reject: (error: Error) => void;
	timeout: ReturnType<typeof setTimeout>;
};

export class ControlClient {
	#peer: RTCPeerConnection | null = null;
	#controlChannel: RTCDataChannel | null = null;
	#reliableChannel: RTCDataChannel | null = null;
	#unreliableChannel: RTCDataChannel | null = null;
	#sessionId: string | null = null;
	#connecting: Promise<void> | null = null;
	#nextRequestId = 1n;
	#pending = new Map<bigint, PendingRequest>();
	#timelinePending = new Map<string, TimelinePending>();
	#playbacks = new Map<string, StoredMediaPlayback>();
	#exportDownloads = new Map<string, ExportDownloadPending>();
	#nextStoredId = 1;
	#serverCapabilities: ServerCapabilities | null = null;
	#capabilityWaiters: CapabilityWaiter[] = [];
	#objectUrls = new Set<string>();
	#capabilityIds: readonly string[] = [];
	#capabilityListeners = new Set<CapabilityListener>();

	onCapabilities(listener: CapabilityListener): () => void {
		this.#capabilityListeners.add(listener);
		listener(this.#capabilityIds);
		return () => this.#capabilityListeners.delete(listener);
	}

	async getRecordings(cameraId: string, date?: string): Promise<RecordingsResponse> {
		const tomorrow = new Date();
		tomorrow.setUTCDate(tomorrow.getUTCDate() + 1);
		tomorrow.setUTCHours(0, 0, 0, 0);
		const timeline = await this.queryStoredTimeline({
			sourceIds: [cameraId],
			startMs: 0,
			endMs: tomorrow.getTime(),
			includeEvents: false,
			includeAttachments: false
		});
		const dates = timelineDates(timeline.ranges);
		const selectedDate = date ?? dates[0] ?? null;
		const segments = selectedDate ? timelineSegments(cameraId, selectedDate, timeline.ranges) : [];
		return { camera_id: cameraId, date: selectedDate, dates, segments };
	}

	async getRecordingsForDate(
		cameraIds: readonly string[],
		date: string,
		signal?: AbortSignal
	): Promise<RecordingsResponse[]> {
		const sourceIds = [...new Set(cameraIds)];
		if (sourceIds.length === 0) return [];
		const { startMs, endMs } = recordingDayWindow(date);
		const timeline = await this.queryStoredTimeline({
			sourceIds,
			startMs,
			endMs,
			includeEvents: false,
			includeAttachments: false,
			signal
		});
		return sourceIds.map((cameraId) => {
			const segments = timelineSegments(cameraId, date, timeline.ranges);
			return {
				camera_id: cameraId,
				date,
				dates: segments.length > 0 ? [date] : [],
				segments
			};
		});
	}

	async getRecordingEvents(
		cameraId: string,
		date: string,
		signal?: AbortSignal
	): Promise<RecordingEventsResponse> {
		const { startMs, endMs } = recordingDayWindow(date);
		const timeline = await this.queryStoredTimeline({
			sourceIds: [cameraId],
			startMs,
			endMs,
			includeEvents: true,
			includeAttachments: true,
			signal
		});
		return { camera_id: cameraId, date, events: timeline.events };
	}

	async openStoredMedia(options: {
		sourceId: string;
		streamId: 'main' | 'sub';
		timestampMs: number;
		endTimeMs: number;
		playing: boolean;
		playbackRate: number;
	}): Promise<StoredMediaPlayback> {
		const storedMediaId = `review-${this.#nextStoredId++}`;
		const playback = new StoredMediaPlayback(
			storedMediaId,
			(timestampMs) => this.refillStoredMedia(storedMediaId, timestampMs),
			(playing, playbackRate) =>
				this.updateStoredMediaPlayback(storedMediaId, playing, playbackRate),
			() => this.closeStoredMedia(storedMediaId)
		);
		this.#playbacks.set(storedMediaId, playback);
		try {
			const command = create(StoredMediaCommandSchema, {
				action: {
					case: 'open',
					value: create(OpenStoredMediaSchema, {
						storedMediaId,
						sourceId: options.sourceId,
						streamId: options.streamId,
						timestamp: timestampFromDate(new Date(options.timestampMs)),
						endTime: timestampFromDate(new Date(options.endTimeMs)),
						mode: StoredMediaMode.PLAYBACK,
						playing: options.playing,
						playbackRate: options.playbackRate,
						mediaChannel: DataChannelKind.RELIABLE_DATA,
						dataPayloadRoutes: [],
						maxBufferDuration: durationFromMs(
							Math.min(300_000, Math.max(1_000, options.endTimeMs - options.timestampMs))
						)
					})
				}
			});
			const result = await this.request({ case: 'storedMediaCommand', value: command });
			if (result.case !== 'storedMediaState') {
				throw new Error('Server returned an unexpected stored media response.');
			}
			playback.configure(result.value);
			return playback;
		} catch (error) {
			this.#playbacks.delete(storedMediaId);
			playback.dispose();
			throw error;
		}
	}

	async getServerCapabilities(): Promise<ServerCapabilities> {
		await this.connect();
		if (this.#serverCapabilities) return this.#serverCapabilities;
		return new Promise((resolve, reject) => {
			const timeout = setTimeout(() => {
				this.#capabilityWaiters = this.#capabilityWaiters.filter(
					(waiter) => waiter.resolve !== resolve
				);
				reject(new Error('WebRTC server capabilities did not arrive.'));
			}, controlTimeoutMs);
			this.#capabilityWaiters.push({ resolve, reject, timeout });
		});
	}

	async getCameras(): Promise<CameraListItem[]> {
		return camerasFromCapabilities(await this.getServerCapabilities());
	}

	async getHealth(signal?: AbortSignal): Promise<ServerHealthResponse> {
		signal?.throwIfAborted();
		const command = create(HealthCommandSchema, {
			action: { case: 'get', value: create(GetHealthSchema) }
		});
		const result = await this.request({ case: 'healthCommand', value: command });
		signal?.throwIfAborted();
		if (result.case !== 'healthResult') {
			throw new Error('Server returned an unexpected health response.');
		}
		return serverHealth(result.value);
	}

	async getCameraDetails(sourceId: string, signal?: AbortSignal): Promise<CameraDetailsResponse> {
		const [cameras, health, motionDetection] = await Promise.all([
			this.getCameras(),
			this.getHealth(signal),
			this.getMotionDetection(sourceId)
		]);
		signal?.throwIfAborted();
		const camera = cameras.find((candidate) => candidate.id === sourceId);
		if (!camera) throw new Error(`Camera '${sourceId}' was not found.`);
		const cameraHealth = health.cameras.find((candidate) => candidate.id === sourceId) ?? null;
		return {
			camera: {
				...camera,
				backend: cameraHealth?.backend ?? camera.backend,
				transport: cameraHealth?.transport ?? camera.transport,
				profiles:
					cameraHealth && cameraHealth.configured_profiles.length > 0
						? cameraHealth.configured_profiles
						: camera.profiles
			},
			health: cameraHealth,
			motion_detection: motionDetection
		};
	}

	async createExport(options: {
		sourceId: string;
		streamId: 'main' | 'sub';
		startMs: number;
		endMs: number;
		allowPartial?: boolean;
		burnInTimestamp?: boolean;
	}): Promise<MediaExportJob> {
		const jobId = `export-${this.#nextStoredId++}`;
		const command = create(ExportCommandSchema, {
			action: {
				case: 'create',
				value: create(CreateExportJobSchema, {
					jobId,
					sourceId: options.sourceId,
					streamId: options.streamId,
					startTime: timestampFromDate(new Date(options.startMs)),
					endTime: timestampFromDate(new Date(options.endMs)),
					allowPartial: options.allowPartial ?? false,
					burnInTimestamp: options.burnInTimestamp ?? false
				})
			}
		});
		return this.exportJobRequest(command);
	}

	async listExports(): Promise<MediaExportJob[]> {
		const command = create(ExportCommandSchema, {
			action: { case: 'list', value: create(ListExportJobsSchema) }
		});
		const result = await this.request({ case: 'exportCommand', value: command });
		if (result.case !== 'exportJobs') {
			throw new Error('Server returned an unexpected export list response.');
		}
		return result.value.jobs.map(mediaExportJob);
	}

	async getExport(jobId: string): Promise<MediaExportJob> {
		const command = create(ExportCommandSchema, {
			action: { case: 'get', value: create(GetExportJobSchema, { jobId }) }
		});
		return this.exportJobRequest(command);
	}

	async cancelExport(jobId: string): Promise<MediaExportJob> {
		const command = create(ExportCommandSchema, {
			action: { case: 'cancel', value: create(CancelExportJobSchema, { jobId }) }
		});
		return this.exportJobRequest(command);
	}

	async retryExport(jobId: string): Promise<MediaExportJob> {
		const command = create(ExportCommandSchema, {
			action: { case: 'retry', value: create(RetryExportJobSchema, { jobId }) }
		});
		return this.exportJobRequest(command);
	}

	async downloadExport(jobId: string): Promise<MediaExportDownload> {
		if (this.#exportDownloads.has(jobId)) {
			throw new Error('Export download is already in progress.');
		}
		const completed = new Promise<MediaExportDownload>((resolve, reject) => {
			const timeout = setTimeout(() => {
				this.#exportDownloads.delete(jobId);
				reject(new Error('Export download timed out.'));
			}, 120_000);
			this.#exportDownloads.set(jobId, {
				job: null,
				expectedChunks: null,
				chunks: [],
				completing: false,
				resolve,
				reject,
				timeout
			});
		});
		try {
			const command = create(ExportCommandSchema, {
				action: {
					case: 'download',
					value: create(DownloadExportSchema, {
						jobId,
						channel: DataChannelKind.RELIABLE_DATA
					})
				}
			});
			const result = await this.request({ case: 'exportCommand', value: command });
			if (
				result.case !== 'exportDownload' ||
				!result.value.job ||
				result.value.channel !== DataChannelKind.RELIABLE_DATA ||
				result.value.chunkCount === 0
			) {
				throw new Error('Server returned an unexpected export download response.');
			}
			const pending = this.#exportDownloads.get(jobId);
			if (!pending) return await completed;
			pending.job = mediaExportJob(result.value.job);
			pending.expectedChunks = result.value.chunkCount;
			if (pending.chunks.length > result.value.chunkCount) {
				throw new Error('Export download chunk count was inconsistent.');
			}
			void this.finishExportDownload(jobId);
			return await completed;
		} catch (error) {
			const pending = this.#exportDownloads.get(jobId);
			if (pending) clearTimeout(pending.timeout);
			this.#exportDownloads.delete(jobId);
			throw error;
		}
	}

	async getMotionDetection(sourceId: string): Promise<MotionDetection> {
		const command = create(CameraControlCommandSchema, {
			action: {
				case: 'getMotionDetection',
				value: create(GetMotionDetectionSchema, { sourceId })
			}
		});
		const result = await this.request({ case: 'cameraControlCommand', value: command });
		if (result.case !== 'motionDetectionResult') {
			throw new Error('Server returned an unexpected motion detection response.');
		}
		return motionDetection(result.value);
	}

	async movePtz(
		sourceId: string,
		movement: { pan: number; tilt: number; zoom: number }
	): Promise<void> {
		await this.ptzRequest(
			create(PtzCommandSchema, {
				sourceId,
				action: {
					case: 'continuous',
					value: create(PtzContinuousSchema, movement)
				}
			})
		);
	}

	async stopPtz(sourceId: string): Promise<void> {
		await this.ptzRequest(
			create(PtzCommandSchema, {
				sourceId,
				action: { case: 'stop', value: create(PtzStopSchema) }
			})
		);
	}

	async getPtzPresets(sourceId: string): Promise<PtzPreset[]> {
		const result = await this.ptzRequest(
			create(PtzCommandSchema, {
				sourceId,
				action: { case: 'listPresets', value: create(PtzPresetListSchema) }
			})
		);
		return result.presets.map((preset) => ({ id: preset.presetId, name: preset.name }));
	}

	async gotoPtzPreset(sourceId: string, presetId: number): Promise<void> {
		await this.ptzRequest(
			create(PtzCommandSchema, {
				sourceId,
				action: {
					case: 'gotoPreset',
					value: create(PtzPresetGotoSchema, { presetId })
				}
			})
		);
	}

	async setMotionDetection(sourceId: string, enabled: boolean): Promise<MotionDetection> {
		const command = create(CameraControlCommandSchema, {
			action: {
				case: 'setMotionDetection',
				value: create(SetMotionDetectionSchema, { sourceId, enabled })
			}
		});
		const result = await this.request({ case: 'cameraControlCommand', value: command });
		if (result.case !== 'motionDetectionResult') {
			throw new Error('Server returned an unexpected motion detection response.');
		}
		return motionDetection(result.value);
	}

	private async ptzRequest(command: import('./proto/webrtc_pb').PtzCommand) {
		const envelope = create(CameraControlCommandSchema, {
			action: { case: 'ptz', value: command }
		});
		const result = await this.request({ case: 'cameraControlCommand', value: envelope });
		if (result.case !== 'ptzResult' || result.value.sourceId !== command.sourceId) {
			throw new Error('Server returned an unexpected PTZ response.');
		}
		return result.value;
	}

	private async exportJobRequest(
		command: import('./proto/webrtc_pb').ExportCommand
	): Promise<MediaExportJob> {
		const result = await this.request({ case: 'exportCommand', value: command });
		if (result.case !== 'exportJob') {
			throw new Error('Server returned an unexpected export job response.');
		}
		return mediaExportJob(result.value);
	}

	async setCameraManufacturer(
		sourceId: string,
		manufacturer: string | null
	): Promise<string | null> {
		const update = create(OptionalStringUpdateSchema, {
			value:
				manufacturer === null
					? { case: 'clear', value: true }
					: { case: 'set', value: manufacturer }
		});
		const command = create(CameraControlCommandSchema, {
			action: {
				case: 'setManufacturer',
				value: create(SetCameraManufacturerSchema, { sourceId, manufacturer: update })
			}
		});
		const result = await this.request({ case: 'cameraControlCommand', value: command });
		if (result.case !== 'cameraManufacturerResult') {
			throw new Error('Server returned an unexpected manufacturer response.');
		}
		return result.value.manufacturer ?? null;
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
		const result = await this.request({ case: 'serverCommand', value: command });
		if (result.case !== 'restartResult' || !result.value.restarting) {
			throw new Error('Server did not acknowledge the restart request.');
		}
	}

	async discoverCameras(subnets: number[]): Promise<DiscoveredCameraSettings[]> {
		const command = create(CameraConfigurationCommandSchema, {
			action: {
				case: 'discover',
				value: create(DiscoverCamerasSchema, { subnets })
			}
		});
		const result = await this.request({ case: 'cameraConfigurationCommand', value: command });
		if (result.case !== 'cameraDiscoveryResult') {
			throw new Error('Server returned an unexpected camera discovery response.');
		}
		return result.value.cameras.map((camera) => ({
			ip: camera.ip,
			brand: camera.brand,
			name: camera.name ?? null,
			model: camera.model ?? null,
			onvif_port: camera.onvifPort ?? null,
			sources: camera.sources,
			configured: camera.configured,
			health: (camera.health ?? null) as DiscoveredCameraSettings['health']
		}));
	}

	async getCameraSettings(): Promise<CameraSettings[]> {
		const command = create(CameraConfigurationCommandSchema, {
			action: { case: 'get', value: create(GetCameraConfigurationsSchema) }
		});
		const result = await this.request({ case: 'cameraConfigurationCommand', value: command });
		if (result.case !== 'cameraConfigurationResult') {
			throw new Error('Server returned an unexpected camera configuration response.');
		}
		return result.value.cameras.map(cameraSettings);
	}

	async updateCamera(
		ip: string,
		update: CameraSettingsUpdate
	): Promise<CameraSettingsUpdateResponse> {
		const payload = create(UpdateCameraConfigurationSchema, {
			ip,
			displayName: stringPatch(update, 'display_name'),
			manufacturer: stringPatch(update, 'manufacturer'),
			username: update.username,
			password: update.password,
			onvifPort: numberPatch(update, 'onvif_port'),
			httpPort: numberPatch(update, 'http_port'),
			mainRtspUrl: stringPatch(update, 'main_rtsp_url'),
			subRtspUrl: stringPatch(update, 'sub_rtsp_url'),
			uid: stringPatch(update, 'uid'),
			backend: update.backend === undefined ? undefined : protoBackend(update.backend),
			transport: update.transport === undefined ? undefined : protoTransport(update.transport)
		});
		const command = create(CameraConfigurationCommandSchema, {
			action: { case: 'update', value: payload }
		});
		const result = await this.request({ case: 'cameraConfigurationCommand', value: command });
		if (result.case !== 'cameraConfigurationResult' || !result.value.camera) {
			throw new Error('Server returned an unexpected camera configuration response.');
		}
		return {
			camera: cameraSettings(result.value.camera),
			restart_required: result.value.restartRequired
		};
	}

	async removeCamera(ip: string): Promise<void> {
		const command = create(CameraConfigurationCommandSchema, {
			action: {
				case: 'remove',
				value: create(RemoveCameraConfigurationSchema, { ip })
			}
		});
		const result = await this.request({ case: 'cameraConfigurationCommand', value: command });
		if (result.case !== 'cameraConfigurationResult' || !result.value.removed) {
			throw new Error('Server did not confirm camera removal.');
		}
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
			longTermMaxGb: BigInt(update.storage.long_term_max_gb)
		});
		const command = create(RuntimeConfigurationCommandSchema, {
			action: {
				case: 'update',
				value: create(UpdateRuntimeConfigurationSchema, {
					host: update.host,
					port: update.port,
					storage,
					moveExistingRecordings: update.move_existing_recordings
				})
			}
		});
		const result = await this.request({ case: 'runtimeConfigurationCommand', value: command });
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
		const result = await this.request({ case: 'runtimeConfigurationCommand', value: command });
		if (result.case !== 'runtimeConfigurationResult' || !result.value.config) {
			throw new Error('Server returned an unexpected runtime configuration response.');
		}
		return runtimeConfiguration(result.value.config);
	}

	async close(): Promise<void> {
		const sessionId = this.release();
		if (sessionId !== null) await deleteSession(sessionId);
	}

	closeOnPageHide(): void {
		const sessionId = this.release();
		if (sessionId === null) return;
		const body = new Blob([JSON.stringify({ session_id: sessionId })], {
			type: 'application/json'
		});
		navigator.sendBeacon('/delete', body);
	}

	private async queryStoredTimeline(options: {
		sourceIds: string[];
		startMs: number;
		endMs: number;
		includeEvents: boolean;
		includeAttachments: boolean;
		signal?: AbortSignal;
	}): Promise<StoredTimelineResult> {
		if (options.signal?.aborted) throw timelineAbortError();
		const queryId = `timeline-${this.#nextStoredId++}`;
		const completed = new Promise<StoredTimelineResult>((resolve, reject) => {
			const timeout = setTimeout(() => {
				this.#timelinePending.delete(queryId);
				reject(new Error('Stored timeline query timed out.'));
			}, controlTimeoutMs);
			this.#timelinePending.set(queryId, {
				ranges: [],
				events: [],
				pages: new Set(),
				attachments: new Map(),
				resolve,
				reject,
				timeout
			});
		});
		let awaitingDelivery = false;
		let aborted = false;
		const abort = () => {
			aborted = true;
			const pending = this.#timelinePending.get(queryId);
			if (pending) {
				clearTimeout(pending.timeout);
				this.#timelinePending.delete(queryId);
				if (awaitingDelivery) pending.reject(timelineAbortError());
			}
			void this.cancelStoredTimelineQuery(queryId).catch(() => undefined);
		};
		options.signal?.addEventListener('abort', abort, { once: true });
		try {
			const command = create(StoredMediaCommandSchema, {
				action: {
					case: 'queryTimeline',
					value: create(QueryStoredMediaTimelineSchema, {
						queryId,
						sourceIds: options.sourceIds,
						startTime: timestampFromDate(new Date(options.startMs)),
						endTime: timestampFromDate(new Date(options.endMs)),
						payloadTypes: [],
						channel: DataChannelKind.RELIABLE_DATA,
						events: options.includeEvents
							? create(StoredMediaEventQuerySchema, {
									eventTypes: [],
									includeAttachments: options.includeAttachments
								})
							: undefined
					})
				}
			});
			const result = await this.request({ case: 'storedMediaCommand', value: command });
			if (
				result.case !== 'storedMediaQueryDelivery' ||
				result.value.queryId !== queryId ||
				result.value.channel !== DataChannelKind.RELIABLE_DATA
			) {
				throw new Error('Server returned an unexpected stored timeline response.');
			}
			if (aborted) throw timelineAbortError();
			awaitingDelivery = true;
			if (options.signal?.aborted) abort();
			return await completed;
		} catch (error) {
			const pending = this.#timelinePending.get(queryId);
			if (pending) clearTimeout(pending.timeout);
			this.#timelinePending.delete(queryId);
			throw error;
		} finally {
			options.signal?.removeEventListener('abort', abort);
		}
	}

	private async cancelStoredTimelineQuery(queryId: string): Promise<void> {
		const command = create(StoredMediaCommandSchema, {
			action: {
				case: 'cancelTimelineQuery',
				value: create(CancelStoredMediaTimelineQuerySchema, { queryId })
			}
		});
		await this.request({ case: 'storedMediaCommand', value: command });
	}

	private async refillStoredMedia(
		storedMediaId: string,
		playbackTimeMs: number
	): Promise<StoredMediaState> {
		const command = create(StoredMediaCommandSchema, {
			action: {
				case: 'refill',
				value: create(RefillStoredMediaSchema, {
					storedMediaId,
					playbackTime: timestampFromDate(new Date(playbackTimeMs))
				})
			}
		});
		const result = await this.request({ case: 'storedMediaCommand', value: command });
		if (result.case !== 'storedMediaState') {
			throw new Error('Server returned an unexpected stored media refill response.');
		}
		return result.value;
	}

	private async updateStoredMediaPlayback(
		storedMediaId: string,
		playing: boolean | undefined,
		playbackRate: number | undefined
	): Promise<StoredMediaState> {
		const command = create(StoredMediaCommandSchema, {
			action: {
				case: 'setPlayback',
				value: create(SetStoredMediaPlaybackSchema, {
					storedMediaId,
					playing,
					playbackRate
				})
			}
		});
		const result = await this.request({ case: 'storedMediaCommand', value: command });
		if (result.case !== 'storedMediaState') {
			throw new Error('Server returned an unexpected stored media playback response.');
		}
		return result.value;
	}

	private async closeStoredMedia(storedMediaId: string): Promise<void> {
		const playback = this.#playbacks.get(storedMediaId);
		this.#playbacks.delete(storedMediaId);
		try {
			const command = create(StoredMediaCommandSchema, {
				action: {
					case: 'close',
					value: create(CloseStoredMediaSchema, { storedMediaId })
				}
			});
			await this.request({ case: 'storedMediaCommand', value: command });
		} finally {
			playback?.dispose();
		}
	}

	private async request(command: Request['command']) {
		await this.connect();
		const channel = this.#controlChannel;
		if (!channel || channel.readyState !== 'open') {
			throw new Error('WebRTC control channel is unavailable.');
		}
		const requestId = this.#nextRequestId;
		this.#nextRequestId += 2n;
		const response = new Promise<ControlResponse>((resolve, reject) => {
			const timeout = setTimeout(() => {
				this.#pending.delete(requestId);
				reject(new Error('WebRTC control request timed out.'));
			}, controlTimeoutMs);
			this.#pending.set(requestId, { resolve, reject, timeout });
		});
		const envelope = create(ControlEnvelopeSchema, {
			message: {
				case: 'request',
				value: create(RequestSchema, { requestId, command })
			}
		});
		channel.send(toBinary(ControlEnvelopeSchema, envelope));
		const reply = await response;
		if (reply.result.case === 'error') throw new Error(reply.result.value.message);
		if (reply.result.case !== 'ok') throw new Error('Server returned an empty control response.');
		return reply.result.value.result;
	}

	private async loggingRequest(command: ReturnType<typeof create<typeof LoggingCommandSchema>>) {
		const result = await this.request({ case: 'loggingCommand', value: command });
		if (result.case !== 'loggingSettingsResult') {
			throw new Error('Server returned an unexpected logging response.');
		}
		return loggingSettings(result.value);
	}

	private connect(): Promise<void> {
		if (this.#controlChannel?.readyState === 'open') return Promise.resolve();
		this.#connecting ??= this.connectNow().finally(() => {
			this.#connecting = null;
		});
		return this.#connecting;
	}

	private async connectNow(): Promise<void> {
		const peer = new RTCPeerConnection();
		this.#peer = peer;
		const control = peer.createDataChannel('control-channel', {
			negotiated: true,
			id: 0,
			ordered: true
		});
		const reliable = peer.createDataChannel('reliable-data', {
			negotiated: true,
			id: 1,
			ordered: true
		});
		const unreliable = peer.createDataChannel('unreliable-data', {
			negotiated: true,
			id: 2,
			ordered: false,
			maxRetransmits: 0
		});
		control.binaryType = 'arraybuffer';
		reliable.binaryType = 'arraybuffer';
		unreliable.binaryType = 'arraybuffer';
		this.#controlChannel = control;
		this.#reliableChannel = reliable;
		this.#unreliableChannel = unreliable;

		const opened = new Promise<void>((resolve, reject) => {
			const timeout = setTimeout(
				() => reject(new Error('WebRTC control channel did not open.')),
				controlTimeoutMs
			);
			control.onopen = () => {
				clearTimeout(timeout);
				resolve();
			};
			control.onerror = () => {
				clearTimeout(timeout);
				reject(new Error('WebRTC control channel failed.'));
			};
		});
		control.onmessage = (event) => this.receive(event);
		reliable.onmessage = (event) => this.receiveData(event);
		control.onclose = () => {
			this.publishCapabilities([]);
			this.failPending('WebRTC control channel closed.');
		};
		reliable.onclose = () => this.failData('WebRTC reliable data channel closed.');
		peer.onconnectionstatechange = () => {
			if (['failed', 'disconnected', 'closed'].includes(peer.connectionState)) {
				this.failPending('WebRTC control connection ended.');
			}
		};

		try {
			const offer = await peer.createOffer();
			await peer.setLocalDescription(offer);
			if (!peer.localDescription) throw new Error('WebRTC offer is unavailable.');
			const session = await createSession(peer.localDescription);
			if (peer !== this.#peer) {
				await deleteSession(session.session_id);
				return;
			}
			this.#sessionId = session.session_id;
			await peer.setRemoteDescription({
				type: session.answer.type as RTCSdpType,
				sdp: session.answer.sdp
			});
			if (control.readyState !== 'open') await opened;
		} catch (error) {
			const sessionId = this.release();
			if (sessionId !== null) await deleteSession(sessionId).catch(() => undefined);
			throw error;
		}
	}

	private receive(event: MessageEvent): void {
		if (!(event.data instanceof ArrayBuffer)) {
			this.failPending('WebRTC control response was not binary.');
			return;
		}
		let envelope;
		try {
			envelope = fromBinary(ControlEnvelopeSchema, new Uint8Array(event.data));
		} catch {
			this.failPending('WebRTC control response was invalid.');
			return;
		}
		if (envelope.message.case === 'notification') {
			if (envelope.message.value.event.case === 'initialCapabilities') {
				const capabilities = envelope.message.value.event.value;
				this.#serverCapabilities = capabilities;
				for (const waiter of this.#capabilityWaiters) {
					clearTimeout(waiter.timeout);
					waiter.resolve(capabilities);
				}
				this.#capabilityWaiters = [];
				this.publishCapabilities(capabilities.capabilityIds);
			}
			if (envelope.message.value.event.case === 'storedMediaState') {
				const state = envelope.message.value.event.value;
				this.#playbacks.get(state.storedMediaId)?.configure(state);
			}
			return;
		}
		if (envelope.message.case !== 'response') return;
		const pending = this.#pending.get(envelope.message.value.requestId);
		if (!pending) return;
		this.#pending.delete(envelope.message.value.requestId);
		clearTimeout(pending.timeout);
		pending.resolve(envelope.message.value);
	}

	private receiveData(event: MessageEvent): void {
		if (!(event.data instanceof ArrayBuffer)) {
			this.failData('WebRTC data message was not binary.');
			return;
		}
		let message;
		try {
			message = fromBinary(MessageSchema, new Uint8Array(event.data));
		} catch {
			this.failData('WebRTC data message was invalid.');
			return;
		}
		if (message.message.case === 'storedMediaQuery') {
			const query = message.message.value.message;
			if (query.case === 'page') this.receiveTimelinePage(query.value);
			if (query.case === 'end') this.finishTimelineQuery(query.value);
			return;
		}
		if (message.message.case === 'event') {
			const eventMessage = message.message.value.message;
			if (eventMessage.case === 'attachment') this.receiveTimelineAttachment(eventMessage.value);
			return;
		}
		if (message.message.case === 'export') {
			const exportMessage = message.message.value.message;
			if (exportMessage.case === 'fileChunk') this.receiveExportChunk(exportMessage.value);
			return;
		}
		if (message.message.case !== 'storedMedia') return;
		const stored = message.message.value.message;
		if (stored.case === 'initialization') {
			this.#playbacks.get(stored.value.storedMediaId)?.receiveInitialization(stored.value);
		}
		if (stored.case === 'fragment') {
			this.#playbacks.get(stored.value.storedMediaId)?.receiveFragment(stored.value);
		}
	}

	private receiveExportChunk(chunk: import('./proto/webrtc_pb').ExportFileChunk): void {
		const pending = this.#exportDownloads.get(chunk.jobId);
		if (!pending) return;
		if (
			chunk.chunkCount === 0 ||
			chunk.chunkIndex >= chunk.chunkCount ||
			(pending.expectedChunks !== null && pending.expectedChunks !== chunk.chunkCount)
		) {
			this.failExportDownload(chunk.jobId, 'Export download chunks were inconsistent.');
			return;
		}
		if (pending.chunks.length === 0) {
			pending.chunks = Array.from<Uint8Array | undefined>({ length: chunk.chunkCount });
		} else if (pending.chunks.length !== chunk.chunkCount) {
			this.failExportDownload(chunk.jobId, 'Export download chunk count changed.');
			return;
		}
		if (pending.chunks[chunk.chunkIndex] !== undefined) {
			this.failExportDownload(chunk.jobId, 'Export download repeated a chunk.');
			return;
		}
		pending.chunks[chunk.chunkIndex] = chunk.payload;
		void this.finishExportDownload(chunk.jobId);
	}

	private async finishExportDownload(jobId: string): Promise<void> {
		const pending = this.#exportDownloads.get(jobId);
		if (
			!pending?.job ||
			pending.completing ||
			pending.expectedChunks === null ||
			pending.chunks.length !== pending.expectedChunks ||
			!pending.chunks.every((chunk) => chunk !== undefined)
		) {
			return;
		}
		pending.completing = true;
		const payload = concatenateChunks(pending.chunks);
		try {
			if (!pending.job.sha256) throw new Error('Export download did not include a checksum.');
			const digest = await crypto.subtle.digest('SHA-256', ownedArrayBuffer(payload));
			if (hexDigest(digest) !== pending.job.sha256.toLowerCase()) {
				throw new Error('Export download checksum did not match the server.');
			}
			if (this.#exportDownloads.get(jobId) !== pending) return;
			this.#exportDownloads.delete(jobId);
			clearTimeout(pending.timeout);
			pending.resolve({
				job: pending.job,
				blob: new Blob([ownedArrayBuffer(payload)], { type: 'video/mp4' })
			});
		} catch (error) {
			this.failExportDownload(
				jobId,
				error instanceof Error ? error.message : 'Export download verification failed.'
			);
		}
	}

	private failExportDownload(jobId: string, message: string): void {
		const pending = this.#exportDownloads.get(jobId);
		if (!pending) return;
		this.#exportDownloads.delete(jobId);
		clearTimeout(pending.timeout);
		pending.reject(new Error(message));
	}

	private receiveTimelinePage(page: import('./proto/webrtc_pb').StoredMediaQueryPage): void {
		const pending = this.#timelinePending.get(page.queryId);
		if (!pending) return;
		pending.pages.add(numeric(page.sequence));
		for (const range of page.availability) {
			if (!range.startTime || !range.endTime) continue;
			pending.ranges.push({
				sourceId: range.sourceId,
				streamId: range.streamId,
				startMs: timestampDate(range.startTime).getTime(),
				endMs: timestampDate(range.endTime).getTime()
			});
		}
		pending.events.push(...page.events);
	}

	private receiveTimelineAttachment(
		attachment: import('./proto/webrtc_pb').EventAttachmentChunk
	): void {
		if (attachment.context.case !== 'queryId') return;
		const pending = this.#timelinePending.get(attachment.context.value);
		if (!pending) return;
		const chunkCount = attachment.chunkCount;
		const chunkIndex = attachment.chunkIndex;
		if (chunkCount === 0 || chunkIndex >= chunkCount) {
			pending.reject(new Error('Stored event attachment chunks were invalid.'));
			this.#timelinePending.delete(attachment.context.value);
			clearTimeout(pending.timeout);
			return;
		}
		const key = `${attachment.eventId}:${attachment.attachmentId}`;
		const accumulator = pending.attachments.get(key) ?? {
			chunkCount,
			chunks: Array.from<Uint8Array | undefined>({ length: chunkCount }),
			contentType: attachment.contentType
		};
		if (
			accumulator.chunkCount !== chunkCount ||
			accumulator.contentType !== attachment.contentType ||
			accumulator.chunks[chunkIndex] !== undefined
		) {
			pending.reject(new Error('Stored event attachment chunks were inconsistent.'));
			this.#timelinePending.delete(attachment.context.value);
			clearTimeout(pending.timeout);
			return;
		}
		accumulator.chunks[chunkIndex] = attachment.payload;
		pending.attachments.set(key, accumulator);
	}

	private finishTimelineQuery(end: import('./proto/webrtc_pb').StoredMediaQueryEnd): void {
		const pending = this.#timelinePending.get(end.queryId);
		if (!pending) return;
		this.#timelinePending.delete(end.queryId);
		clearTimeout(pending.timeout);
		const attachments = [...pending.attachments.values()].filter((attachment) =>
			attachment.chunks.every((chunk) => chunk !== undefined)
		);
		if (
			pending.pages.size !== numeric(end.pageCount) ||
			attachments.length !== numeric(end.attachmentCount)
		) {
			pending.reject(new Error('Stored timeline delivery was incomplete.'));
			return;
		}
		pending.resolve({
			ranges: pending.ranges,
			events: pending.events.map((event) =>
				recordingEvent(event, pending.attachments, (url) => this.#objectUrls.add(url))
			)
		});
	}

	private failData(message: string): void {
		for (const pending of this.#timelinePending.values()) {
			clearTimeout(pending.timeout);
			pending.reject(new Error(message));
		}
		this.#timelinePending.clear();
		for (const [jobId] of this.#exportDownloads) this.failExportDownload(jobId, message);
		for (const playback of this.#playbacks.values()) playback.fail(message);
	}

	private failPending(message: string): void {
		for (const pending of this.#pending.values()) {
			clearTimeout(pending.timeout);
			pending.reject(new Error(message));
		}
		this.#pending.clear();
	}

	private publishCapabilities(capabilityIds: readonly string[]): void {
		this.#capabilityIds = [...capabilityIds];
		for (const listener of this.#capabilityListeners) listener(this.#capabilityIds);
	}

	private release(): string | null {
		const sessionId = this.#sessionId;
		this.#sessionId = null;
		this.#controlChannel?.close();
		this.#reliableChannel?.close();
		this.#unreliableChannel?.close();
		this.#peer?.close();
		this.#controlChannel = null;
		this.#reliableChannel = null;
		this.#unreliableChannel = null;
		this.#peer = null;
		this.#serverCapabilities = null;
		for (const waiter of this.#capabilityWaiters) {
			clearTimeout(waiter.timeout);
			waiter.reject(new Error('WebRTC control connection closed.'));
		}
		this.#capabilityWaiters = [];
		this.failData('WebRTC control connection closed.');
		for (const playback of this.#playbacks.values()) playback.dispose();
		this.#playbacks.clear();
		for (const url of this.#objectUrls) URL.revokeObjectURL(url);
		this.#objectUrls.clear();
		this.publishCapabilities([]);
		this.failPending('WebRTC control connection closed.');
		return sessionId;
	}
}

type CompletedStoredObject = {
	generation: bigint;
	payload: Uint8Array;
};

type StoredChunkAccumulator = ChunkAccumulator & {
	generation: bigint;
	deliveredThroughMs?: number;
};

export class StoredMediaPlayback {
	readonly id: string;
	readonly url: string;
	anchorTimeMs = 0;
	initialOffsetSeconds = 0;
	error: string | null = null;

	#mediaSource = new MediaSource();
	#sourceBuffer: SourceBuffer | null = null;
	#contentType: string | null = null;
	#generation = 0n;
	#chunks = new Map<string, StoredChunkAccumulator>();
	#completed: CompletedStoredObject[] = [];
	#appendQueue: Uint8Array[] = [];
	#refill: (timestampMs: number) => Promise<StoredMediaState>;
	#updatePlayback: (
		playing: boolean | undefined,
		playbackRate: number | undefined
	) => Promise<StoredMediaState>;
	#close: () => Promise<void>;
	#maxBufferMs = 0;
	#deliveredThroughMs = 0;
	#endTimeMs: number | null = null;
	#refillInFlight = false;
	#ended = false;
	#playing = false;
	#playbackRate = 1;
	#closed = false;

	constructor(
		id: string,
		refill: (timestampMs: number) => Promise<StoredMediaState>,
		updatePlayback: (
			playing: boolean | undefined,
			playbackRate: number | undefined
		) => Promise<StoredMediaState>,
		close: () => Promise<void>
	) {
		this.id = id;
		this.#refill = refill;
		this.#updatePlayback = updatePlayback;
		this.#close = close;
		this.url = URL.createObjectURL(this.#mediaSource);
		this.#mediaSource.addEventListener('sourceopen', () => this.initializeSourceBuffer(), {
			once: true
		});
	}

	configure(state: StoredMediaState): void {
		if (!state.delivery || !state.requestedTime || !state.fragmentTime) {
			throw new Error('Server returned incomplete stored media state.');
		}
		this.#generation = state.generation;
		this.#contentType = state.delivery.contentType;
		this.anchorTimeMs = timestampDate(state.fragmentTime).getTime();
		this.#maxBufferMs = state.delivery.maxBufferDuration
			? protoDurationMs(state.delivery.maxBufferDuration)
			: 0;
		this.#endTimeMs = state.endTime ? timestampDate(state.endTime).getTime() : null;
		this.#ended = state.status === StoredMediaStatus.ENDED;
		this.#playing = state.playing;
		this.#playbackRate = state.playbackRate;
		this.initialOffsetSeconds = Math.max(
			0,
			(timestampDate(state.requestedTime).getTime() - this.anchorTimeMs) / 1_000
		);
		this.#completed = this.#completed.filter((object) => object.generation === this.#generation);
		for (const [key, chunks] of this.#chunks) {
			if (chunks.generation !== this.#generation) this.#chunks.delete(key);
		}
		this.#appendQueue.push(...this.#completed.map((object) => object.payload));
		this.#completed = [];
		this.initializeSourceBuffer();
		if (this.#endTimeMs !== null && this.#mediaSource.readyState === 'open') {
			this.#mediaSource.duration = Math.max(0, (this.#endTimeMs - this.anchorTimeMs) / 1_000);
		}
		this.flushAppendQueue();
		this.finishIfEnded();
	}

	receiveInitialization(initialization: StoredMediaInitialization): void {
		this.receiveChunks(
			`init:${initialization.generation}:${initialization.initializationId}`,
			initialization.generation,
			initialization.chunkIndex,
			initialization.chunkCount,
			initialization.contentType,
			initialization.payload,
			undefined
		);
	}

	receiveFragment(fragment: StoredMediaFragment): void {
		const deliveredThroughMs =
			fragment.startTime && fragment.duration
				? timestampDate(fragment.startTime).getTime() + protoDurationMs(fragment.duration)
				: undefined;
		this.receiveChunks(
			`fragment:${fragment.generation}:${fragment.sequence}`,
			fragment.generation,
			fragment.chunkIndex,
			fragment.chunkCount,
			this.#contentType ?? 'video/mp4',
			fragment.payload,
			deliveredThroughMs
		);
	}

	observe(currentTimeSeconds: number): void {
		if (
			this.#closed ||
			this.#ended ||
			!this.#playing ||
			this.#refillInFlight ||
			this.#maxBufferMs <= 0 ||
			this.#deliveredThroughMs <= this.anchorTimeMs
		) {
			return;
		}
		const playbackTimeMs = this.anchorTimeMs + Math.max(0, currentTimeSeconds) * 1_000;
		if (this.#deliveredThroughMs - playbackTimeMs > this.#maxBufferMs / 2) return;
		this.#refillInFlight = true;
		void this.#refill(playbackTimeMs)
			.then((state) => this.configure(state))
			.catch((error) =>
				this.fail(error instanceof Error ? error.message : 'Unable to refill stored media.')
			)
			.finally(() => {
				this.#refillInFlight = false;
			});
	}

	setPlaying(playing: boolean): void {
		if (this.#closed || playing === this.#playing) return;
		const previous = this.#playing;
		this.#playing = playing;
		void this.#updatePlayback(playing, undefined)
			.then((state) => this.acceptPlaybackState(state))
			.catch((error) => {
				if (this.#playing === playing) this.#playing = previous;
				this.fail(error instanceof Error ? error.message : 'Unable to update stored playback.');
			});
	}

	setPlaybackRate(playbackRate: number): void {
		if (
			this.#closed ||
			!Number.isFinite(playbackRate) ||
			playbackRate <= 0 ||
			playbackRate === this.#playbackRate
		) {
			return;
		}
		const previous = this.#playbackRate;
		this.#playbackRate = playbackRate;
		void this.#updatePlayback(undefined, playbackRate)
			.then((state) => this.acceptPlaybackState(state))
			.catch((error) => {
				if (this.#playbackRate === playbackRate) this.#playbackRate = previous;
				this.fail(error instanceof Error ? error.message : 'Unable to update stored playback.');
			});
	}

	private acceptPlaybackState(state: StoredMediaState): void {
		if (state.generation !== this.#generation) {
			this.configure(state);
			return;
		}
		this.#playing = state.playing;
		this.#playbackRate = state.playbackRate;
		this.#ended = state.status === StoredMediaStatus.ENDED;
		this.finishIfEnded();
	}

	async close(): Promise<void> {
		if (this.#closed) return;
		await this.#close();
	}

	fail(message: string): void {
		this.error = message;
	}

	dispose(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#chunks.clear();
		this.#completed = [];
		this.#appendQueue = [];
		this.#refillInFlight = false;
		URL.revokeObjectURL(this.url);
	}

	private receiveChunks(
		key: string,
		generation: bigint,
		chunkIndex: number,
		chunkCount: number,
		contentType: string,
		payload: Uint8Array,
		deliveredThroughMs: number | undefined
	): void {
		if (this.#closed || chunkCount === 0 || chunkIndex >= chunkCount) return;
		const accumulator = this.#chunks.get(key) ?? {
			generation,
			chunkCount,
			chunks: Array.from<Uint8Array | undefined>({ length: chunkCount }),
			contentType,
			deliveredThroughMs
		};
		if (
			accumulator.generation !== generation ||
			accumulator.chunkCount !== chunkCount ||
			accumulator.contentType !== contentType ||
			accumulator.deliveredThroughMs !== deliveredThroughMs ||
			accumulator.chunks[chunkIndex] !== undefined
		) {
			this.fail('Stored media chunks were inconsistent.');
			return;
		}
		accumulator.chunks[chunkIndex] = payload;
		this.#chunks.set(key, accumulator);
		if (!accumulator.chunks.every((chunk) => chunk !== undefined)) return;
		this.#chunks.delete(key);
		if (accumulator.deliveredThroughMs !== undefined) {
			this.#deliveredThroughMs = Math.max(this.#deliveredThroughMs, accumulator.deliveredThroughMs);
		}
		const completed = { generation, payload: concatenateChunks(accumulator.chunks) };
		if (this.#generation === 0n) {
			this.#completed.push(completed);
			return;
		}
		if (generation !== this.#generation) return;
		this.#appendQueue.push(completed.payload);
		this.flushAppendQueue();
	}

	private initializeSourceBuffer(): void {
		if (
			this.#sourceBuffer ||
			!this.#contentType ||
			this.#mediaSource.readyState !== 'open' ||
			this.#closed
		) {
			return;
		}
		if (!MediaSource.isTypeSupported(this.#contentType)) {
			this.fail(`Browser does not support ${this.#contentType}.`);
			return;
		}
		try {
			this.#sourceBuffer = this.#mediaSource.addSourceBuffer(this.#contentType);
			this.#sourceBuffer.mode = 'sequence';
			this.#sourceBuffer.addEventListener('updateend', () => {
				this.flushAppendQueue();
				this.finishIfEnded();
			});
			this.#sourceBuffer.addEventListener('error', () =>
				this.fail('Browser rejected stored media bytes.')
			);
		} catch (error) {
			this.fail(error instanceof Error ? error.message : 'Unable to initialize stored playback.');
		}
	}

	private flushAppendQueue(): void {
		const sourceBuffer = this.#sourceBuffer;
		if (!sourceBuffer || sourceBuffer.updating || this.#closed) {
			return;
		}
		if (this.#appendQueue.length === 0) {
			this.finishIfEnded();
			return;
		}
		const payload = this.#appendQueue.shift();
		if (!payload) return;
		try {
			sourceBuffer.appendBuffer(ownedArrayBuffer(payload));
		} catch (error) {
			this.fail(error instanceof Error ? error.message : 'Unable to append stored media bytes.');
		}
	}

	private finishIfEnded(): void {
		if (
			!this.#ended ||
			this.#closed ||
			this.#mediaSource.readyState !== 'open' ||
			this.#sourceBuffer?.updating ||
			this.#appendQueue.length > 0 ||
			this.#chunks.size > 0 ||
			(this.#endTimeMs !== null && this.#deliveredThroughMs < this.#endTimeMs)
		) {
			return;
		}
		try {
			this.#mediaSource.endOfStream();
		} catch (error) {
			this.fail(error instanceof Error ? error.message : 'Unable to finish stored playback.');
		}
	}
}

function timelineDates(ranges: StoredTimelineRange[]): string[] {
	const dates = new Set<string>();
	for (const range of ranges) {
		let day = Date.parse(`${new Date(range.startMs).toISOString().slice(0, 10)}T00:00:00Z`);
		const last = Math.max(day, range.endMs - 1);
		while (day <= last) {
			dates.add(new Date(day).toISOString().slice(0, 10));
			day += 86_400_000;
		}
	}
	return [...dates].toSorted().toReversed();
}

function recordingDayWindow(date: string): { startMs: number; endMs: number } {
	const startMs = Date.parse(`${date}T00:00:00Z`);
	if (!Number.isFinite(startMs) || new Date(startMs).toISOString().slice(0, 10) !== date) {
		throw new Error('Recording date is invalid.');
	}
	return { startMs, endMs: startMs + 86_400_000 };
}

function timelineAbortError(): DOMException {
	return new DOMException('Stored timeline query was cancelled.', 'AbortError');
}

function timelineSegments(
	cameraId: string,
	date: string,
	ranges: StoredTimelineRange[]
): RecordingSegment[] {
	const dayStart = Date.parse(`${date}T00:00:00Z`);
	const dayEnd = dayStart + 86_400_000;
	return ranges
		.filter(
			(range) =>
				range.sourceId === cameraId &&
				(range.streamId === 'main' || range.streamId === 'sub') &&
				range.startMs < dayEnd &&
				range.endMs > dayStart
		)
		.map((range) => {
			const startTimeMs = Math.max(dayStart, range.startMs);
			const endTimeMs = Math.min(dayEnd, range.endMs);
			const hour = new Date(startTimeMs).toISOString().slice(11, 13);
			return {
				stream: range.streamId as 'main' | 'sub',
				date,
				hour,
				filename: `${startTimeMs}-${endTimeMs}.mp4`,
				url: `stored:${cameraId}:${range.streamId}:${startTimeMs}:${endTimeMs}`,
				start_time_ms: startTimeMs,
				end_time_ms: endTimeMs,
				duration_ms: endTimeMs - startTimeMs
			};
		})
		.toSorted((left, right) => right.start_time_ms - left.start_time_ms);
}

function recordingEvent(
	event: ProtoEvent,
	attachments: Map<string, ChunkAccumulator>,
	onObjectUrl: (url: string) => void
): RecordingEvent {
	const descriptor = event.attachments.find(
		(attachment) => attachment.attachmentType === 'thumbnail'
	);
	const attachment = descriptor
		? attachments.get(`${event.eventId}:${descriptor.attachmentId}`)
		: undefined;
	const thumbnailUrl = attachment?.chunks.every((chunk) => chunk !== undefined)
		? URL.createObjectURL(
				new Blob([ownedArrayBuffer(concatenateChunks(attachment.chunks))], {
					type: attachment.contentType
				})
			)
		: null;
	if (thumbnailUrl) onObjectUrl(thumbnailUrl);
	return {
		id: event.eventId,
		source: event.origin === EventOrigin.KEEPPEEK ? 'keeppeek' : 'camera',
		kind: event.eventType,
		start_time_ms: event.startTime ? timestampDate(event.startTime).getTime() : 0,
		end_time_ms: event.endTime ? timestampDate(event.endTime).getTime() : null,
		confidence: event.confidence ?? null,
		bbox: event.boundingBox
			? [
					event.boundingBox.x,
					event.boundingBox.y,
					event.boundingBox.width,
					event.boundingBox.height
				]
			: null,
		zone: event.zone ?? null,
		thumbnail_url: thumbnailUrl
	};
}

function concatenateChunks(chunks: Array<Uint8Array | undefined>): Uint8Array {
	const complete = chunks.filter((chunk): chunk is Uint8Array => chunk !== undefined);
	const length = complete.reduce((total, chunk) => total + chunk.byteLength, 0);
	const payload = new Uint8Array(length);
	let offset = 0;
	for (const chunk of complete) {
		payload.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return payload;
}

function ownedArrayBuffer(payload: Uint8Array): ArrayBuffer {
	return Uint8Array.from(payload).buffer;
}

function hexDigest(payload: ArrayBuffer): string {
	return Array.from(new Uint8Array(payload), (byte) => byte.toString(16).padStart(2, '0')).join('');
}

function protoDurationMs(duration: import('@bufbuild/protobuf/wkt').Duration): number {
	return Number(duration.seconds) * 1_000 + duration.nanos / 1_000_000;
}

function mediaExportJob(job: ProtoExportJob): MediaExportJob {
	if (!job.requestedStartTime || !job.requestedEndTime) {
		throw new Error('Server returned incomplete export range evidence.');
	}
	if (job.streamId !== 'main' && job.streamId !== 'sub') {
		throw new Error(`Server returned an unexpected export stream '${job.streamId}'.`);
	}
	const status: MediaExportJobStatus =
		job.status === ExportJobStatus.RUNNING
			? 'running'
			: job.status === ExportJobStatus.PARTIAL
				? 'partial'
				: job.status === ExportJobStatus.READY
					? 'ready'
					: job.status === ExportJobStatus.FAILED
						? 'failed'
						: job.status === ExportJobStatus.CANCELLED
							? 'cancelled'
							: job.status === ExportJobStatus.EXPIRED
								? 'expired'
								: (() => {
										throw new Error('Server returned an unspecified export status.');
									})();
	return {
		id: job.jobId,
		sourceId: job.sourceId,
		streamId: job.streamId,
		requestedStartMs: timestampDate(job.requestedStartTime).getTime(),
		requestedEndMs: timestampDate(job.requestedEndTime).getTime(),
		alignedStartMs: job.alignedStartTime ? timestampDate(job.alignedStartTime).getTime() : null,
		status,
		progress: Math.min(1, job.progressPerMille / 1_000),
		bytesWritten: numeric(job.bytesWritten),
		estimatedBytes: job.estimatedBytes === undefined ? null : numeric(job.estimatedBytes),
		fileName: job.fileName ?? null,
		sha256: job.sha256 ?? null,
		expiresAtMs: job.expiresAt ? timestampDate(job.expiresAt).getTime() : null,
		missingRanges: job.missingRanges.map((range) => {
			if (!range.startTime || !range.endTime) {
				throw new Error('Server returned an incomplete export gap.');
			}
			return {
				startMs: timestampDate(range.startTime).getTime(),
				endMs: timestampDate(range.endTime).getTime()
			};
		}),
		error: job.error ?? null,
		retryable: job.retryable,
		burnInTimestamp: job.burnInTimestamp
	};
}

function motionDetection(result: MotionDetectionResult): MotionDetection {
	return {
		supported: result.supported,
		controllable: result.controllable,
		enabled: result.enabled ?? null,
		error: result.error ?? null
	};
}

function loggingSettings(result: LoggingSettingsResult): LoggingSettings {
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

function numeric(value: bigint): number {
	return Number(value > BigInt(Number.MAX_SAFE_INTEGER) ? BigInt(Number.MAX_SAFE_INTEGER) : value);
}

function stringPatch<T extends CameraSettingsUpdate>(
	update: T,
	key: keyof Pick<
		CameraSettingsUpdate,
		'display_name' | 'manufacturer' | 'main_rtsp_url' | 'sub_rtsp_url' | 'uid'
	>
) {
	if (!Object.hasOwn(update, key)) return undefined;
	const value = update[key];
	return create(OptionalStringUpdateSchema, {
		value: value === null ? { case: 'clear', value: true } : { case: 'set', value: value ?? '' }
	});
}

function numberPatch<T extends CameraSettingsUpdate>(
	update: T,
	key: keyof Pick<CameraSettingsUpdate, 'onvif_port' | 'http_port'>
) {
	if (!Object.hasOwn(update, key)) return undefined;
	const value = update[key];
	return create(OptionalUint32UpdateSchema, {
		value: value === null ? { case: 'clear', value: true } : { case: 'set', value: value ?? 0 }
	});
}

function protoBackend(backend: CameraBackend): ProtoCameraBackend {
	if (backend === 'retina') return ProtoCameraBackend.RETINA;
	if (backend === 'reo-proto') return ProtoCameraBackend.REO_PROTO;
	return ProtoCameraBackend.AUTO;
}

function protoTransport(transport: CameraTransport): ProtoCameraTransport {
	return transport === 'udp' ? ProtoCameraTransport.UDP : ProtoCameraTransport.TCP;
}

function cameraSettings(camera: import('./proto/webrtc_pb').CameraSettings): CameraSettings {
	return {
		id: camera.id,
		ip: camera.ip,
		display_name: camera.displayName ?? null,
		manufacturer_override: camera.manufacturerOverride ?? null,
		username_configured: camera.usernameConfigured,
		password_configured: camera.passwordConfigured,
		onvif_port: camera.onvifPort ?? null,
		http_port: camera.httpPort ?? null,
		main_rtsp_url: camera.mainRtspUrl ?? null,
		sub_rtsp_url: camera.subRtspUrl ?? null,
		uid_configured: camera.uidConfigured,
		backend:
			camera.backend === ProtoCameraBackend.RETINA
				? 'retina'
				: camera.backend === ProtoCameraBackend.REO_PROTO
					? 'reo-proto'
					: 'auto',
		transport: camera.transport === ProtoCameraTransport.UDP ? 'udp' : 'tcp',
		health: (camera.health ?? null) as CameraSettings['health'],
		model: camera.model ?? null
	};
}

function runtimeConfiguration(config: SanitizedRuntimeConfiguration): SanitizedConfig {
	if (!config.storage || !config.recordingEstimate) {
		throw new Error('Server returned incomplete runtime configuration evidence.');
	}
	return {
		host: config.host,
		port: config.port,
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
			long_term_max_gb: numeric(config.storage.longTermMaxGb)
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

function serverHealth(health: ServerHealthSnapshot): ServerHealthResponse {
	const { totals, system, storage, webrtc } = health;
	if (!totals || !system || !storage || !webrtc) {
		throw new Error('Server returned incomplete health evidence.');
	}
	const { process, memory, load } = system;
	const demand = storage.demand;
	if (!process || !memory || !load || !demand) {
		throw new Error('Server returned incomplete health evidence.');
	}
	return {
		status: health.status === 'healthy' ? 'healthy' : 'degraded',
		generated_at_ms: numeric(health.generatedAtMs),
		uptime_seconds: numeric(health.uptimeSeconds),
		version: health.version,
		totals: {
			configured_cameras: numeric(totals.configuredCameras),
			reporting_cameras: numeric(totals.reportingCameras),
			configured_video_streams: numeric(totals.configuredVideoStreams),
			reporting_video_streams: numeric(totals.reportingVideoStreams),
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
			catalog_bytes: optionalNumber(storage.catalogBytes),
			catalog: storage.catalog
				? {
						recording_files: numeric(storage.catalog.recordingFiles),
						finalized_files: numeric(storage.catalog.finalizedFiles),
						active_files: numeric(storage.catalog.activeFiles),
						fragments: numeric(storage.catalog.fragments),
						fragment_bytes: numeric(storage.catalog.fragmentBytes),
						events: numeric(storage.catalog.events),
						open_events: numeric(storage.catalog.openEvents),
						event_thumbnails: numeric(storage.catalog.eventThumbnails)
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
			message: issue.message
		}))
	};
}

function cameraHealth(camera: ServerHealthSnapshot['cameras'][number]): CameraHealth {
	return {
		id: camera.id,
		ip: camera.ip,
		name: camera.name,
		manufacturer: camera.manufacturer ?? null,
		model: camera.model ?? null,
		firmware_version: camera.firmwareVersion ?? null,
		backend: camera.backend,
		transport: camera.transport,
		state: camera.state as CameraHealth['state'],
		lifecycle: camera.lifecycle ?? null,
		last_error: camera.lastError ?? null,
		configured_profiles: camera.configuredProfiles.map(healthProfile),
		streams: camera.streams.map(streamHealth)
	};
}

function healthProfile(
	profile: ServerHealthSnapshot['cameras'][number]['configuredProfiles'][number]
): ProfileSummary {
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
		report_age_ms: numeric(stream.reportAgeMs)
	};
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

function camerasFromCapabilities(capabilities: ServerCapabilities): CameraListItem[] {
	return capabilities.cameras.map((camera) => {
		const live = capabilities.sourceSessions.find(
			(session) => session.sourceId === camera.sourceId
		);
		const stored = capabilities.storedMediaSources.find(
			(source) => source.sourceId === camera.sourceId
		);
		const streamIds = new Set<'main' | 'sub'>();
		for (const variant of live?.video?.variants ?? []) {
			if (variant.variantId === 'main' || variant.variantId === 'sub') {
				streamIds.add(variant.variantId);
			}
		}
		for (const stream of stored?.streams ?? []) {
			if (stream.streamId === 'main' || stream.streamId === 'sub') {
				streamIds.add(stream.streamId);
			}
		}
		const profiles = [...streamIds]
			.toSorted((left) => (left === 'main' ? -1 : 1))
			.map((stream) => {
				const variant = live?.video?.variants.find((candidate) => candidate.variantId === stream);
				const video = variant?.format?.format;
				return {
					name: `${stream}Stream`,
					stream,
					encoding: variant?.codec?.name.toLowerCase() ?? null,
					resolution:
						video?.case === 'video' && video.value.width > 0 && video.value.height > 0
							? `${video.value.width}x${video.value.height}`
							: null,
					framerate: null,
					bitrate_kbps:
						variant && variant.nominalBitrateBps > 0n
							? numeric(variant.nominalBitrateBps / 1_000n)
							: null,
					gop: null,
					h264_profile: null,
					audio: null
				};
			});
		return {
			id: camera.sourceId,
			ip: camera.ip ?? camera.sourceId,
			name: camera.displayName === camera.sourceId ? null : camera.displayName,
			manufacturer: camera.manufacturer ?? null,
			model: camera.model ?? null,
			firmware_version: camera.firmwareVersion ?? null,
			serial_number: camera.serialNumber ?? null,
			hardware_id: camera.hardwareId ?? null,
			hostname: camera.hostname ?? null,
			mac_address: camera.macAddress ?? null,
			is_reolink: camera.isReolink,
			web_url: camera.webUrl,
			ports: {
				http: camera.httpPort ?? null,
				https: camera.httpsPort ?? null,
				rtsp: camera.rtspPort ?? null,
				onvif: camera.onvifPort ?? null
			},
			capabilities: {
				ptz: camera.ptz?.supported ?? false,
				audio: camera.deviceCapabilities?.audio ?? false,
				events: camera.deviceCapabilities?.events ?? false,
				recording: camera.deviceCapabilities?.recording ?? false,
				analytics: camera.deviceCapabilities?.analytics ?? false,
				imaging: camera.deviceCapabilities?.imaging ?? false,
				two_way_audio: camera.deviceCapabilities?.twoWayAudio ?? false
			},
			profiles
		};
	});
}
