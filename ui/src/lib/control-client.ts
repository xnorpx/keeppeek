import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import { durationFromMs, timestampDate, timestampFromDate } from '@bufbuild/protobuf/wkt';
import { createSession, deleteSession } from './api';
import { emitTimelinePerformanceEvent } from './timeline-observability';
import {
	AcknowledgeNotificationSchema,
	ActivateNotificationRuleSchema,
	CameraControlCommandSchema,
	CameraConfigurationCommandSchema,
	CameraBackend as ProtoCameraBackend,
	CameraRecordingMode as ProtoCameraRecordingMode,
	CameraTransport as ProtoCameraTransport,
	CancelCameraDiscoverySchema,
	CancelEventSearchMediaSchema,
	CancelEventSearchQuerySchema,
	CancelStoredMediaTimelineQuerySchema,
	ClearNotificationSchema,
	ClearNotificationsSchema,
	ControlEnvelopeSchema,
	CloseStoredMediaSchema,
	DataChannelKind,
	DiscoverCamerasSchema,
	DeleteNotificationRuleSchema,
	DownloadExportSchema,
	EventSearchCommandSchema,
	EventSearchField,
	EventImageFilter as ProtoEventImageFilter,
	EventSearchMediaObjectSchema,
	EventMetadataSearchSchema,
	EventTextSearchSchema,
	EventOrigin,
	ExportCommandSchema,
	ExportJobStatus,
	GetExportJobSchema,
	GetCameraCatalogSchema,
	GetCameraOnboardingDefaultsSchema,
	GetCameraConfigurationsSchema,
	GetCameraDiscoverySchema,
	GetAccessKeySchema,
	GetHealthSchema,
	GetLoggingSettingsSchema,
	GetMotionDetectionSchema,
	GetNotificationHistorySchema,
	GetNotificationInboxSchema,
	GetRuntimeConfigurationSchema,
	FetchEventSearchMediaSchema,
	ListExportJobsSchema,
	ListNotificationRulesSchema,
	LoggingCommandSchema,
	HealthCommandSchema,
	MessageSchema,
	MarkNotificationSeenSchema,
	NotificationRuleCommandSchema,
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
	ProbeStorageSchema,
	ProbeCameraStreamsSchema,
	RemoveCameraConfigurationSchema,
	RuntimeConfigurationCommandSchema,
	RuntimeStorageConfigurationSchema,
	RequestSchema,
	RestartServerSchema,
	RotateAccessKeySchema,
	SearchCameraCatalogSchema,
	SaveNotificationRuleDraftSchema,
	ServerCommandSchema,
	SeekStoredMediaSchema,
	SetCameraManufacturerSchema,
	SetLoggingFilterSchema,
	SetMotionDetectionSchema,
	SetStoredMediaPlaybackSchema,
	StoredMediaCommandSchema,
	StoredMediaEventQuerySchema,
	StoredMediaMode,
	StoredMediaStatus,
	TestNotificationRuleSchema,
	CancelExportJobSchema,
	CreateExportJobSchema,
	QueryStoredMediaTimelineSchema,
	QueryEventsSchema,
	StoredMediaObjectRepresentation,
	UpdateCameraConfigurationSchema,
	UpdateRuntimeConfigurationSchema,
	type LoggingSettingsResult,
	type SanitizedRuntimeConfiguration,
	type ServerHealthSnapshot,
	type Event as ProtoEvent,
	type EventSearchHit as ProtoEventSearchHit,
	type EventSearchMediaChunk,
	type ExportJob as ProtoExportJob,
	type HealthProfileSummary,
	type ServerCapabilities,
	type StoredMediaFragment,
	type StoredMediaInitialization,
	type StoredMediaKeyFrame,
	type StoredMediaState,
	type MotionDetectionResult,
	type NotificationDeliveryAttempt as ProtoNotificationDeliveryAttempt,
	type NotificationHistoryEvent as ProtoNotificationHistoryEvent,
	type NotificationHistoryGroup as ProtoNotificationHistoryGroup,
	type NotificationInbox as ProtoNotificationInbox,
	type NotificationItem as ProtoNotificationItem,
	type NotificationRuleRecord as ProtoNotificationRuleRecord,
	type Request,
	type QueryEvents,
	type Response as ControlResponse
} from './proto/webrtc_pb';
import {
	parseNotificationRuleDefinition,
	type NotificationChannel,
	type NotificationClearScope,
	type NotificationDeliveryAttempt,
	type NotificationHistoryEvent,
	type NotificationHistoryGroup,
	type NotificationInbox,
	type NotificationItem,
	type NotificationRuleDefinition,
	type NotificationRuleRecord,
	type NotificationSeverity,
	type NotificationStage,
	type NotificationTestResult
} from './notifications';
import type {
	MotionDetection,
	RecordingEvent,
	RecordingEventsResponse,
	RecordingSegment,
	RecordingsResponse
} from './types';
import type { LoggingSettings } from './types';
import type {
	CameraCatalogCamera,
	CameraCatalogInfo,
	CameraOnboardingDefaults,
	CameraStreamProbeResult,
	DiscoveredCameraSettings
} from './types';
import type {
	CameraBackend,
	CameraRecordingMode,
	CameraDetailsResponse,
	CameraListItem,
	CameraSettings,
	CameraSettingsUpdate,
	CameraSettingsUpdateResponse,
	CameraTransport
} from './types';
import type { SanitizedConfig, SettingsConfigUpdate, SettingsConfigUpdateResponse } from './types';
import type {
	CameraHealth,
	CameraHealthDimensions,
	CameraHealthReason,
	CameraHealthState,
	ProfileSummary,
	ServerHealthResponse,
	StreamHealth,
	StreamHealthDimensions
} from './types';
import type { StorageWriteProbe } from './first-run';

const controlTimeoutMs = 10_000;
const discoveryTimeoutMs = 8 * 60_000;
const streamProbeTimeoutMs = 30_000;
const maxTimelineAttachmentBytes = 1_048_576;
const maxTimelineInFlightAttachmentBytes = 8 * 1_048_576;
const maxEventKeyframeBytes = 4 * 1_048_576;

type PendingRequest = {
	resolve: (response: ControlResponse) => void;
	reject: (error: Error) => void;
	timeout: ReturnType<typeof setTimeout>;
};

type CapabilityListener = (capabilityIds: readonly string[]) => void;

export type PtzPreset = { id: number; name: string };

export class NotificationConflictError extends Error {
	constructor(
		message: string,
		readonly activeRevision: bigint,
		readonly draftRevision: bigint
	) {
		super(message);
		this.name = 'NotificationConflictError';
	}
}

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

export type EventPreviewKeyframe = {
	sourceId: string;
	streamId: string;
	recordingId: string;
	fragmentSequence: bigint;
	eventTimeMs: number;
	fragmentStartMs: number;
	byteLength: number;
};

export type EventPreviewHit = {
	eventId: string;
	sourceId: string;
	eventType: string;
	origin: RecordingEvent['source'];
	startMs: number;
	endMs: number | null;
	confidence: number | null;
	bbox: [number, number, number, number] | null;
	zone: string | null;
	text: string | null;
	hasImageAttachment: boolean;
	previewStartMs: number;
	previewEndMs: number;
	keyframes: EventPreviewKeyframe[];
	keyframesTruncated: boolean;
};

export type EventPreviewPage = {
	hits: EventPreviewHit[];
	nextPageToken: string;
	candidatesTruncated: boolean;
};

export type EventMetadataSearchOptions = {
	eventIds?: readonly string[];
	sourceIds: readonly string[];
	streamId: 'main' | 'sub';
	startMs: number;
	endMs: number;
	eventTypes?: readonly string[];
	origins?: readonly RecordingEvent['source'][];
	zones?: readonly string[];
	minimumConfidence?: number;
	image?: 'all' | 'with' | 'without';
	text?: string;
	pageSize?: number;
	pageToken?: string;
	includePreviewKeyframes?: boolean;
	signal?: AbortSignal;
};

export type EncodedEventKeyframe = {
	contentType: string;
	codec: string;
	width: number;
	height: number;
	decoderConfig: Uint8Array;
	nalLengthSize: number;
	payload: Uint8Array;
};

export type StoredMediaKeyFramePreview = EncodedEventKeyframe & {
	storedMediaId: string;
	generation: bigint;
	timestampMs: number;
	configurationRevision: bigint;
};

export type StoredMediaStartupPhase = 'metadata' | 'initialization' | 'first-fragment';

export type StoredMediaStartupEvent = {
	phase: StoredMediaStartupPhase;
	generation: bigint;
	contentType: string;
};

export type StoredTimelineRange = {
	sourceId: string;
	streamId: string;
	startMs: number;
	endMs: number;
};

export type StoredTimelineResult = {
	ranges: StoredTimelineRange[];
	events: RecordingEvent[];
};

export type StoredTimelineQueryOptions = {
	sourceIds: readonly string[];
	startMs: number;
	endMs: number;
	availabilityBucketMs?: number;
	eventTypes?: readonly string[];
	includeEvents: boolean;
	includeAttachments: boolean;
	includeAvailability?: boolean;
	signal?: AbortSignal;
	onPage?: (page: StoredTimelineResult) => void;
};

type TimelinePending = {
	ranges: StoredTimelineRange[];
	events: ProtoEvent[];
	pages: Set<number>;
	attachments: Map<string, TimelineAttachmentAccumulator>;
	onPage?: (page: StoredTimelineResult) => void;
	resolve: (result: StoredTimelineResult) => void;
	reject: (error: Error) => void;
	timeout: ReturnType<typeof setTimeout>;
};

type EventSearchPending = {
	hits: ProtoEventSearchHit[];
	sequences: Set<number>;
	resolve: (page: EventPreviewPage) => void;
	reject: (error: Error) => void;
	timeout: ReturnType<typeof setTimeout>;
};

type EventMediaAccumulator = {
	chunkCount: number;
	chunks: Array<Uint8Array | undefined>;
	byteLength: number;
	receivedBytes: number;
	contentType: string;
	codec: string;
	width: number;
	height: number;
	decoderConfig: Uint8Array;
	nalLengthSize: number;
};

type EventMediaPending = {
	objectIds: Set<string>;
	objects: Map<string, EventMediaAccumulator>;
	resolve: (objects: Map<string, EncodedEventKeyframe>) => void;
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

type TimelineAttachmentAccumulator = ChunkAccumulator & {
	byteCount: number;
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
	#eventSearchPending = new Map<string, EventSearchPending>();
	#eventMediaPending = new Map<string, EventMediaPending>();
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

	releaseObjectUrl(url: string): void {
		if (!this.#objectUrls.delete(url)) return;
		URL.revokeObjectURL(url);
	}

	async getRecordings(cameraId: string, date?: string): Promise<RecordingsResponse> {
		const dates = await this.getRecordingDates(cameraId);
		const selectedDate = date ?? dates[0] ?? null;
		if (!selectedDate) return { camera_id: cameraId, date: null, dates, segments: [] };
		const [recordings] = await this.getRecordingsForDate([cameraId], selectedDate);
		return { ...recordings!, dates };
	}

	async getRecordingDates(cameraId: string, signal?: AbortSignal): Promise<string[]> {
		const tomorrow = new Date();
		tomorrow.setUTCDate(tomorrow.getUTCDate() + 1);
		tomorrow.setUTCHours(0, 0, 0, 0);
		const timeline = await this.queryStoredTimeline({
			sourceIds: [cameraId],
			startMs: 0,
			endMs: tomorrow.getTime(),
			availabilityBucketMs: 86_400_000,
			includeEvents: false,
			includeAttachments: false,
			signal
		});
		return timelineDates(timeline.ranges);
	}

	async getRecordingsForDate(
		cameraIds: readonly string[],
		date: string,
		signal?: AbortSignal,
		onPage?: (recordings: RecordingsResponse[]) => void
	): Promise<RecordingsResponse[]> {
		const { startMs, endMs } = recordingDayWindow(date);
		return this.getRecordingsInRange(cameraIds, date, startMs, endMs, signal, onPage);
	}

	async getRecordingsInRange(
		cameraIds: readonly string[],
		date: string,
		startMs: number,
		endMs: number,
		signal?: AbortSignal,
		onPage?: (recordings: RecordingsResponse[]) => void
	): Promise<RecordingsResponse[]> {
		const sourceIds = [...new Set(cameraIds)];
		if (sourceIds.length === 0) return [];
		const day = recordingDayWindow(date);
		const boundedStartMs = Math.max(day.startMs, startMs);
		const boundedEndMs = Math.min(day.endMs, endMs);
		if (boundedStartMs >= boundedEndMs) return recordingsForSources(sourceIds, date, []);
		const streamedRanges: StoredTimelineRange[] = [];
		const timeline = await this.queryStoredTimeline({
			sourceIds,
			startMs: boundedStartMs,
			endMs: boundedEndMs,
			includeEvents: false,
			includeAttachments: false,
			signal,
			onPage: onPage
				? (page) => {
						streamedRanges.push(...page.ranges);
						onPage(recordingsForSources(sourceIds, date, streamedRanges));
					}
				: undefined
		});
		return recordingsForSources(sourceIds, date, timeline.ranges);
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
			includeAttachments: false,
			signal
		});
		return { camera_id: cameraId, date, events: timeline.events };
	}

	async searchEventPreviews(options: {
		sourceId: string;
		streamId: 'main' | 'sub';
		eventType: string;
		startMs: number;
		endMs: number;
		pageToken?: string;
		signal?: AbortSignal;
	}): Promise<EventPreviewPage> {
		return this.runEventSearch(options.signal, (queryId) =>
			create(QueryEventsSchema, {
				queryId,
				search: {
					case: 'text',
					value: create(EventTextSearchSchema, {
						query: options.eventType,
						field: EventSearchField.EVENT_TYPE
					})
				},
				sourceId: options.sourceId,
				streamId: options.streamId,
				startTime: timestampFromDate(new Date(options.startMs)),
				endTime: timestampFromDate(new Date(options.endMs)),
				previewBefore: durationFromMs(5_000),
				previewAfter: durationFromMs(10_000),
				pageSize: 128,
				channel: DataChannelKind.RELIABLE_DATA,
				pageToken: options.pageToken ?? ''
			})
		);
	}

	async searchEventMetadata(options: EventMetadataSearchOptions): Promise<EventPreviewPage> {
		const image =
			options.image === 'with'
				? ProtoEventImageFilter.WITH_IMAGE
				: options.image === 'without'
					? ProtoEventImageFilter.WITHOUT_IMAGE
					: ProtoEventImageFilter.ANY;
		return this.runEventSearch(options.signal, (queryId) =>
			create(QueryEventsSchema, {
				queryId,
				search: {
					case: 'metadata',
					value: create(EventMetadataSearchSchema, {
						eventIds: [...(options.eventIds ?? [])],
						sourceIds: [...options.sourceIds],
						eventTypes: [...(options.eventTypes ?? [])],
						origins: (options.origins ?? []).map((origin) =>
							origin === 'keeppeek' ? EventOrigin.KEEPPEEK : EventOrigin.CAMERA
						),
						zones: [...(options.zones ?? [])],
						minimumConfidence: options.minimumConfidence,
						image,
						text: options.text?.trim() || undefined,
						includePreviewKeyframes: options.includePreviewKeyframes ?? false
					})
				},
				streamId: options.streamId,
				startTime: timestampFromDate(new Date(options.startMs)),
				endTime: timestampFromDate(new Date(options.endMs)),
				previewBefore: durationFromMs(5_000),
				previewAfter: durationFromMs(10_000),
				pageSize: options.pageSize ?? 50,
				channel: DataChannelKind.RELIABLE_DATA,
				pageToken: options.pageToken ?? ''
			})
		);
	}

	private async runEventSearch(
		signal: AbortSignal | undefined,
		query: (queryId: string) => QueryEvents
	): Promise<EventPreviewPage> {
		if (signal?.aborted) throw timelineAbortError();
		const queryId = `event-search-${this.#nextStoredId++}`;
		const completed = new Promise<EventPreviewPage>((resolve, reject) => {
			const timeout = setTimeout(() => {
				this.#eventSearchPending.delete(queryId);
				reject(new Error('Event search timed out.'));
			}, controlTimeoutMs);
			this.#eventSearchPending.set(queryId, {
				hits: [],
				sequences: new Set(),
				resolve,
				reject,
				timeout
			});
		});
		let awaitingDelivery = false;
		let aborted = false;
		const abort = () => {
			aborted = true;
			const pending = this.#eventSearchPending.get(queryId);
			if (pending) {
				clearTimeout(pending.timeout);
				this.#eventSearchPending.delete(queryId);
				if (awaitingDelivery) pending.reject(timelineAbortError());
			}
			void this.cancelEventSearchQuery(queryId).catch(() => undefined);
		};
		signal?.addEventListener('abort', abort, { once: true });
		try {
			const command = create(EventSearchCommandSchema, {
				action: {
					case: 'query',
					value: query(queryId)
				}
			});
			const result = await this.request({ case: 'eventSearchCommand', value: command });
			if (
				result.case !== 'eventSearchDelivery' ||
				result.value.queryId !== queryId ||
				result.value.channel !== DataChannelKind.RELIABLE_DATA
			) {
				throw new Error('Server returned an unexpected event search response.');
			}
			if (aborted) throw timelineAbortError();
			awaitingDelivery = true;
			if (signal?.aborted) abort();
			return await completed;
		} catch (error) {
			const pending = this.#eventSearchPending.get(queryId);
			if (pending) clearTimeout(pending.timeout);
			this.#eventSearchPending.delete(queryId);
			throw error;
		} finally {
			signal?.removeEventListener('abort', abort);
		}
	}

	async fetchEventPreviewKeyframe(
		keyframe: EventPreviewKeyframe,
		signal?: AbortSignal
	): Promise<EncodedEventKeyframe> {
		if (signal?.aborted) throw timelineAbortError();
		const transferId = `event-media-${this.#nextStoredId++}`;
		const objectId = `keyframe-${this.#nextStoredId++}`;
		const completed = new Promise<Map<string, EncodedEventKeyframe>>((resolve, reject) => {
			const timeout = setTimeout(() => {
				this.#eventMediaPending.delete(transferId);
				reject(new Error('Event keyframe transfer timed out.'));
			}, controlTimeoutMs);
			this.#eventMediaPending.set(transferId, {
				objectIds: new Set([objectId]),
				objects: new Map(),
				resolve,
				reject,
				timeout
			});
		});
		let awaitingDelivery = false;
		let aborted = false;
		const abort = () => {
			aborted = true;
			const pending = this.#eventMediaPending.get(transferId);
			if (pending) {
				clearTimeout(pending.timeout);
				this.#eventMediaPending.delete(transferId);
				if (awaitingDelivery) pending.reject(timelineAbortError());
			}
			void this.cancelEventSearchMedia(transferId).catch(() => undefined);
		};
		signal?.addEventListener('abort', abort, { once: true });
		try {
			const object = create(EventSearchMediaObjectSchema, {
				objectId,
				sourceId: keyframe.sourceId,
				streamId: keyframe.streamId,
				recordingId: keyframe.recordingId,
				fragmentSequence: keyframe.fragmentSequence,
				representation: StoredMediaObjectRepresentation.ENCODED_KEYFRAME
			});
			const command = create(EventSearchCommandSchema, {
				action: {
					case: 'fetchMedia',
					value: create(FetchEventSearchMediaSchema, {
						transferId,
						objects: [object],
						channel: DataChannelKind.RELIABLE_DATA
					})
				}
			});
			const result = await this.request({ case: 'eventSearchCommand', value: command });
			if (
				result.case !== 'eventSearchMediaDelivery' ||
				result.value.transferId !== transferId ||
				result.value.channel !== DataChannelKind.RELIABLE_DATA ||
				result.value.objectCount !== 1
			) {
				throw new Error('Server returned an unexpected event keyframe response.');
			}
			if (aborted) throw timelineAbortError();
			awaitingDelivery = true;
			if (signal?.aborted) abort();
			const objects = await completed;
			const media = objects.get(objectId);
			if (!media) throw new Error('Event keyframe transfer did not contain its requested object.');
			return media;
		} catch (error) {
			const pending = this.#eventMediaPending.get(transferId);
			if (pending) clearTimeout(pending.timeout);
			this.#eventMediaPending.delete(transferId);
			throw error;
		} finally {
			signal?.removeEventListener('abort', abort);
		}
	}

	async openStoredMedia(options: {
		sourceId: string;
		streamId: 'main' | 'sub';
		timestampMs: number;
		endTimeMs: number;
		playing: boolean;
		playbackRate: number;
		mode?: 'scrub' | 'playback';
		signal?: AbortSignal;
		openTimeoutMs?: number;
	}): Promise<StoredMediaPlayback> {
		options.signal?.throwIfAborted();
		const storedMediaId = `review-${this.#nextStoredId++}`;
		const playback = new StoredMediaPlayback(
			storedMediaId,
			options.sourceId,
			options.streamId,
			(timestampMs) => this.seekStoredMedia(storedMediaId, timestampMs),
			(timestampMs) => this.refillStoredMedia(storedMediaId, timestampMs),
			(playing, playbackRate, mode) =>
				this.updateStoredMediaPlayback(storedMediaId, playing, playbackRate, mode),
			() => this.closeStoredMedia(storedMediaId)
		);
		this.#playbacks.set(storedMediaId, playback);
		try {
			const mode = options.mode === 'scrub' ? StoredMediaMode.SCRUB : StoredMediaMode.PLAYBACK;
			const command = create(StoredMediaCommandSchema, {
				action: {
					case: 'open',
					value: create(OpenStoredMediaSchema, {
						storedMediaId,
						sourceId: options.sourceId,
						streamId: options.streamId,
						timestamp: timestampFromDate(new Date(options.timestampMs)),
						endTime: timestampFromDate(new Date(options.endTimeMs)),
						mode,
						playing: mode === StoredMediaMode.SCRUB ? false : options.playing,
						playbackRate: options.playbackRate,
						mediaChannel: DataChannelKind.RELIABLE_DATA,
						dataPayloadRoutes: [],
						maxBufferDuration: durationFromMs(10_000)
					})
				}
			});
			const result = await this.request(
				{ case: 'storedMediaCommand', value: command },
				options.openTimeoutMs ?? 4_000,
				options.signal
			);
			if (result.case !== 'storedMediaState') {
				throw new Error('Server returned an unexpected stored media response.');
			}
			playback.configure(result.value);
			return playback;
		} catch (error) {
			this.#playbacks.delete(storedMediaId);
			playback.dispose();
			if (options.signal?.aborted) {
				void this.closeStoredMedia(storedMediaId).catch(() => undefined);
			}
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

	async listNotificationRules(): Promise<NotificationRuleRecord[]> {
		const command = create(NotificationRuleCommandSchema, {
			action: { case: 'listRules', value: create(ListNotificationRulesSchema) }
		});
		const result = await this.notificationRequest(command);
		if (result.case !== 'rules') {
			throw new Error('Server returned an unexpected notification rule list response.');
		}
		return result.value.rules.map(notificationRuleRecord);
	}

	async saveNotificationRuleDraft(
		rule: NotificationRuleDefinition,
		expectedDraftRevision: bigint
	): Promise<NotificationRuleRecord> {
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'saveDraft',
				value: create(SaveNotificationRuleDraftSchema, {
					definitionJson: JSON.stringify(rule),
					expectedDraftRevision
				})
			}
		});
		return this.notificationRuleMutation(command);
	}

	async activateNotificationRule(
		ruleId: string,
		expectedActiveRevision: bigint,
		expectedDraftRevision: bigint
	): Promise<NotificationRuleRecord> {
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'activate',
				value: create(ActivateNotificationRuleSchema, {
					ruleId,
					expectedActiveRevision,
					expectedDraftRevision
				})
			}
		});
		return this.notificationRuleMutation(command);
	}

	async deleteNotificationRule(
		ruleId: string,
		expectedActiveRevision: bigint,
		expectedDraftRevision: bigint
	): Promise<void> {
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'delete',
				value: create(DeleteNotificationRuleSchema, {
					ruleId,
					expectedActiveRevision,
					expectedDraftRevision
				})
			}
		});
		const result = await this.notificationRequest(command);
		if (result.case !== 'mutation' || result.value.logicalId !== ruleId) {
			throw new Error('Server returned an unexpected notification rule deletion response.');
		}
	}

	async testNotificationRule(ruleId: string): Promise<NotificationTestResult> {
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'test',
				value: create(TestNotificationRuleSchema, { ruleId })
			}
		});
		const result = await this.notificationRequest(command);
		if (result.case !== 'test') {
			throw new Error('Server returned an unexpected notification test response.');
		}
		return {
			matchedRules: result.value.matchedRules,
			createdNotifications: result.value.createdNotifications,
			queuedAttempts: result.value.queuedAttempts
		};
	}

	async getNotificationInbox(limit = 100): Promise<NotificationInbox> {
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'getInbox',
				value: create(GetNotificationInboxSchema, { limit })
			}
		});
		const result = await this.notificationRequest(command);
		if (result.case !== 'inbox') {
			throw new Error('Server returned an unexpected notification inbox response.');
		}
		return notificationInbox(result.value);
	}

	async getNotificationHistory(limit = 100): Promise<NotificationHistoryGroup[]> {
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'getHistory',
				value: create(GetNotificationHistorySchema, { limit })
			}
		});
		const result = await this.notificationRequest(command);
		if (result.case !== 'history') {
			throw new Error('Server returned an unexpected notification history response.');
		}
		return result.value.groups.map(notificationHistoryGroup);
	}

	async markNotificationSeen(logicalId: string): Promise<void> {
		await this.notificationReceiptMutation(
			logicalId,
			create(NotificationRuleCommandSchema, {
				action: {
					case: 'markSeen',
					value: create(MarkNotificationSeenSchema, { logicalId })
				}
			})
		);
	}

	async acknowledgeNotification(logicalId: string): Promise<void> {
		await this.notificationReceiptMutation(
			logicalId,
			create(NotificationRuleCommandSchema, {
				action: {
					case: 'acknowledge',
					value: create(AcknowledgeNotificationSchema, { logicalId })
				}
			})
		);
	}

	async clearNotification(logicalId: string): Promise<void> {
		await this.notificationReceiptMutation(
			logicalId,
			create(NotificationRuleCommandSchema, {
				action: {
					case: 'clear',
					value: create(ClearNotificationSchema, { logicalId })
				}
			})
		);
	}

	async clearNotifications(scope: NotificationClearScope): Promise<bigint> {
		const wireScope =
			scope.kind === 'all'
				? ({ case: 'all', value: true } as const)
				: scope.kind === 'rule'
					? ({ case: 'ruleId', value: scope.ruleId } as const)
					: ({ case: 'beforeMs', value: BigInt(scope.beforeMs) } as const);
		const command = create(NotificationRuleCommandSchema, {
			action: {
				case: 'clearScope',
				value: create(ClearNotificationsSchema, { scope: wireScope })
			}
		});
		const result = await this.notificationRequest(command);
		if (result.case !== 'cleared') {
			throw new Error('Server returned an unexpected notification clear response.');
		}
		return result.value.clearedCount;
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

	async revealAccessKey(): Promise<string> {
		const command = create(ServerCommandSchema, {
			action: { case: 'getAccessKey', value: create(GetAccessKeySchema) }
		});
		const result = await this.request({ case: 'serverCommand', value: command });
		if (result.case !== 'accessKeyResult' || result.value.rotated || !result.value.accessKey) {
			throw new Error('Server returned an unexpected access key response.');
		}
		return result.value.accessKey;
	}

	async rotateAccessKey(): Promise<string> {
		const command = create(ServerCommandSchema, {
			action: { case: 'rotateAccessKey', value: create(RotateAccessKeySchema) }
		});
		const result = await this.request({ case: 'serverCommand', value: command });
		if (result.case !== 'accessKeyResult' || !result.value.rotated || !result.value.accessKey) {
			throw new Error('Server did not return the rotated access key.');
		}
		return result.value.accessKey;
	}

	async getCameraCatalog(): Promise<CameraCatalogInfo> {
		const command = create(CameraConfigurationCommandSchema, {
			action: { case: 'getCatalog', value: create(GetCameraCatalogSchema) }
		});
		const result = await this.request({ case: 'cameraConfigurationCommand', value: command });
		if (result.case !== 'cameraCatalogInfo') {
			throw new Error('Server returned an unexpected camera catalog response.');
		}
		return cameraCatalogInfo(result.value);
	}

	async getCameraOnboardingDefaults(): Promise<CameraOnboardingDefaults> {
		const command = create(CameraConfigurationCommandSchema, {
			action: {
				case: 'getOnboardingDefaults',
				value: create(GetCameraOnboardingDefaultsSchema)
			}
		});
		const result = await this.request({ case: 'cameraConfigurationCommand', value: command });
		if (result.case !== 'cameraOnboardingDefaults') {
			throw new Error('Server returned an unexpected camera onboarding defaults response.');
		}
		return {
			username_configured: result.value.usernameConfigured,
			password_configured: result.value.passwordConfigured,
			networks: result.value.networks.map((network) => ({
				cidr: network.cidr,
				interface_name: network.interfaceName,
				preferred: network.preferred
			}))
		};
	}

	async searchCameraCatalog(
		query: string,
		options: { limit?: number; ip?: string } = {}
	): Promise<CameraCatalogCamera[]> {
		const command = create(CameraConfigurationCommandSchema, {
			action: {
				case: 'searchCatalog',
				value: create(SearchCameraCatalogSchema, {
					query,
					limit: options.limit,
					ip: options.ip
				})
			}
		});
		const result = await this.request({ case: 'cameraConfigurationCommand', value: command });
		if (result.case !== 'cameraCatalogSearchResult') {
			throw new Error('Server returned an unexpected camera catalog search response.');
		}
		return result.value.cameras.map(cameraCatalogCamera);
	}

	async discoverCameras(
		networks: string[],
		options: {
			signal?: AbortSignal;
			onProgress?: (cameras: DiscoveredCameraSettings[]) => void;
		} = {}
	): Promise<DiscoveredCameraSettings[]> {
		if (options.signal?.aborted) throw timelineAbortError();
		const discoveryId = `camera-discovery-${Date.now()}-${this.#nextStoredId++}`;
		const command = create(CameraConfigurationCommandSchema, {
			action: {
				case: 'discover',
				value: create(DiscoverCamerasSchema, { networks, discoveryId })
			}
		});
		let stopped = false;
		const completion = this.request(
			{ case: 'cameraConfigurationCommand', value: command },
			discoveryTimeoutMs
		);
		void completion.catch(() => {});
		let rejectAborted!: (error: Error) => void;
		const aborted = new Promise<never>((_, reject) => (rejectAborted = reject));
		const abort = () => {
			if (stopped) return;
			stopped = true;
			void this.cancelCameraDiscovery(discoveryId).catch(() => {});
			rejectAborted(timelineAbortError());
		};
		options.signal?.addEventListener('abort', abort, { once: true });
		const poll = (async () => {
			while (!stopped) {
				await new Promise((resolve) => setTimeout(resolve, 150));
				if (stopped) return;
				try {
					const snapshot = await this.getCameraDiscovery(discoveryId);
					options.onProgress?.(discoveredCameras(snapshot));
					if (snapshot.complete || snapshot.cancelled) return;
				} catch {
					// The first poll may arrive before the background handler registers its task.
				}
			}
		})();
		let result: Awaited<typeof completion>;
		try {
			result = await Promise.race([completion, aborted]);
		} finally {
			stopped = true;
			options.signal?.removeEventListener('abort', abort);
			await poll;
		}
		if (result.case !== 'cameraDiscoveryResult') {
			throw new Error('Server returned an unexpected camera discovery response.');
		}
		const cameras = discoveredCameras(result.value);
		options.onProgress?.(cameras);
		return cameras;
	}

	private async getCameraDiscovery(discoveryId: string) {
		const command = create(CameraConfigurationCommandSchema, {
			action: {
				case: 'getDiscovery',
				value: create(GetCameraDiscoverySchema, { discoveryId })
			}
		});
		const result = await this.request({ case: 'cameraConfigurationCommand', value: command });
		if (result.case !== 'cameraDiscoveryResult') {
			throw new Error('Server returned an unexpected camera discovery progress response.');
		}
		return result.value;
	}

	private async cancelCameraDiscovery(discoveryId: string): Promise<void> {
		const command = create(CameraConfigurationCommandSchema, {
			action: {
				case: 'cancelDiscovery',
				value: create(CancelCameraDiscoverySchema, { discoveryId })
			}
		});
		await this.request({ case: 'cameraConfigurationCommand', value: command });
	}

	async probeCameraStreams(input: {
		ip: string;
		username: string;
		password: string;
		onvif_port: number | null;
		main_rtsp_url?: string | null;
		sub_rtsp_url?: string | null;
		transport?: CameraTransport;
		query_onvif?: boolean;
	}): Promise<CameraStreamProbeResult> {
		const command = create(CameraConfigurationCommandSchema, {
			action: {
				case: 'probeStreams',
				value: create(ProbeCameraStreamsSchema, {
					ip: input.ip,
					username: input.username,
					password: input.password,
					onvifPort: input.onvif_port ?? undefined,
					mainRtspUrl: input.main_rtsp_url ?? undefined,
					subRtspUrl: input.sub_rtsp_url ?? undefined,
					transport: input.transport === undefined ? undefined : protoTransport(input.transport),
					queryOnvif: input.query_onvif
				})
			}
		});
		const result = await this.request(
			{ case: 'cameraConfigurationCommand', value: command },
			streamProbeTimeoutMs
		);
		if (result.case !== 'cameraStreamProbeResult') {
			throw new Error('Server returned an unexpected camera stream probe response.');
		}
		return {
			main_rtsp_url: result.value.mainRtspUrl ?? null,
			sub_rtsp_url: result.value.subRtspUrl ?? null,
			onvif_port: result.value.onvifPort ?? null,
			manufacturer: result.value.manufacturer ?? null,
			model: result.value.model ?? null,
			firmware_version: result.value.firmwareVersion ?? null,
			serial_number: result.value.serialNumber ?? null,
			hardware_id: result.value.hardwareId ?? null,
			profiles: result.value.profiles.map(healthProfile),
			streams: result.value.streams.map((stream) => ({
				stream: stream.stream === 'sub' ? 'sub' : 'main',
				verified: stream.verified,
				codec: stream.codec ?? null,
				resolution: stream.resolution ?? null,
				declared_fps: stream.declaredFps ?? null,
				frames_received: stream.framesReceived,
				keyframe_received: stream.keyframeReceived,
				elapsed_ms: Number(stream.elapsedMs),
				error: stream.error ?? null
			})),
			onvif_error: result.value.onvifError ?? null
		};
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
			transport: update.transport === undefined ? undefined : protoTransport(update.transport),
			recordGenericMotionEvents: update.record_generic_motion_events,
			recordingMode:
				update.recording_mode === undefined
					? undefined
					: protoCameraRecordingMode(update.recording_mode),
			eventRecordingDurationSecs: update.event_recording_duration_secs
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

	async probeStorage(path: string): Promise<StorageWriteProbe> {
		const command = create(RuntimeConfigurationCommandSchema, {
			action: { case: 'probeStorage', value: create(ProbeStorageSchema, { path }) }
		});
		const result = await this.request({ case: 'runtimeConfigurationCommand', value: command });
		if (result.case !== 'storageWriteProbeResult') {
			throw new Error('Server returned an unexpected storage write probe response.');
		}
		return { writable: result.value.writable, detail: result.value.detail };
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

	async queryStoredTimeline(options: StoredTimelineQueryOptions): Promise<StoredTimelineResult> {
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
				onPage: options.onPage,
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
						sourceIds: [...options.sourceIds],
						startTime: timestampFromDate(new Date(options.startMs)),
						endTime: timestampFromDate(new Date(options.endMs)),
						payloadTypes: [],
						availabilityBucket:
							options.availabilityBucketMs && options.availabilityBucketMs > 0
								? durationFromMs(options.availabilityBucketMs)
								: undefined,
						channel: DataChannelKind.RELIABLE_DATA,
						omitAvailability: options.includeAvailability === false,
						events: options.includeEvents
							? create(StoredMediaEventQuerySchema, {
									eventTypes: [...(options.eventTypes ?? [])],
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

	private async cancelEventSearchQuery(queryId: string): Promise<void> {
		const command = create(EventSearchCommandSchema, {
			action: {
				case: 'cancelQuery',
				value: create(CancelEventSearchQuerySchema, { queryId })
			}
		});
		await this.request({ case: 'eventSearchCommand', value: command });
	}

	private async cancelEventSearchMedia(transferId: string): Promise<void> {
		const command = create(EventSearchCommandSchema, {
			action: {
				case: 'cancelMedia',
				value: create(CancelEventSearchMediaSchema, { transferId })
			}
		});
		await this.request({ case: 'eventSearchCommand', value: command });
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

	private async seekStoredMedia(
		storedMediaId: string,
		timestampMs: number
	): Promise<StoredMediaState> {
		const command = create(StoredMediaCommandSchema, {
			action: {
				case: 'seek',
				value: create(SeekStoredMediaSchema, {
					storedMediaId,
					timestamp: timestampFromDate(new Date(timestampMs))
				})
			}
		});
		const result = await this.request({ case: 'storedMediaCommand', value: command });
		if (result.case !== 'storedMediaState') {
			throw new Error('Server returned an unexpected stored media seek response.');
		}
		return result.value;
	}

	private async updateStoredMediaPlayback(
		storedMediaId: string,
		playing: boolean | undefined,
		playbackRate: number | undefined,
		mode: StoredMediaMode | undefined
	): Promise<StoredMediaState> {
		const command = create(StoredMediaCommandSchema, {
			action: {
				case: 'setPlayback',
				value: create(SetStoredMediaPlaybackSchema, {
					storedMediaId,
					playing,
					playbackRate,
					mode
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

	private async request(
		command: Request['command'],
		timeoutMs = controlTimeoutMs,
		signal?: AbortSignal
	) {
		signal?.throwIfAborted();
		await this.connect();
		signal?.throwIfAborted();
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
			}, timeoutMs);
			this.#pending.set(requestId, { resolve, reject, timeout });
		});
		const abort = () => {
			const pending = this.#pending.get(requestId);
			if (!pending) return;
			clearTimeout(pending.timeout);
			this.#pending.delete(requestId);
			pending.reject(timelineAbortError());
		};
		signal?.addEventListener('abort', abort, { once: true });
		const envelope = create(ControlEnvelopeSchema, {
			message: {
				case: 'request',
				value: create(RequestSchema, { requestId, command })
			}
		});
		try {
			channel.send(toBinary(ControlEnvelopeSchema, envelope));
			const reply = await response;
			if (reply.result.case === 'error') {
				const conflict = reply.result.value.details.find(
					(detail) => detail.typeUrl === 'type.keeppeek.dev/notification-rule-conflict.v1'
				);
				if (conflict) {
					try {
						const detail: unknown = JSON.parse(new TextDecoder().decode(conflict.value));
						if (
							detail &&
							typeof detail === 'object' &&
							'active_revision' in detail &&
							'draft_revision' in detail
						) {
							throw new NotificationConflictError(
								reply.result.value.message,
								BigInt(String(detail.active_revision)),
								BigInt(String(detail.draft_revision))
							);
						}
					} catch (error) {
						if (error instanceof NotificationConflictError) throw error;
					}
				}
				throw new Error(reply.result.value.message);
			}
			if (reply.result.case !== 'ok') throw new Error('Server returned an empty control response.');
			return reply.result.value.result;
		} finally {
			signal?.removeEventListener('abort', abort);
		}
	}

	private async notificationRequest(
		command: ReturnType<typeof create<typeof NotificationRuleCommandSchema>>
	) {
		const result = await this.request({ case: 'notificationRuleCommand', value: command });
		if (result.case !== 'notificationRuleResult' || !result.value.result.case) {
			throw new Error('Server returned an unexpected notification response.');
		}
		return result.value.result;
	}

	private async notificationRuleMutation(
		command: ReturnType<typeof create<typeof NotificationRuleCommandSchema>>
	): Promise<NotificationRuleRecord> {
		const result = await this.notificationRequest(command);
		if (result.case !== 'rule') {
			throw new Error('Server returned an unexpected notification rule response.');
		}
		return notificationRuleRecord(result.value);
	}

	private async notificationReceiptMutation(
		logicalId: string,
		command: ReturnType<typeof create<typeof NotificationRuleCommandSchema>>
	): Promise<void> {
		const result = await this.notificationRequest(command);
		if (result.case !== 'mutation' || result.value.logicalId !== logicalId) {
			throw new Error('Server returned an unexpected notification receipt response.');
		}
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
			if (
				peer === this.#peer &&
				['failed', 'disconnected', 'closed'].includes(peer.connectionState)
			) {
				const sessionId = this.releaseTransport(false);
				if (sessionId !== null) void deleteSession(sessionId).catch(() => undefined);
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
		if (message.message.case === 'eventSearch') {
			const search = message.message.value.message;
			if (search.case === 'result') this.receiveEventSearchResult(search.value);
			if (search.case === 'queryEnd') this.finishEventSearch(search.value);
			if (search.case === 'mediaChunk') this.receiveEventMediaChunk(search.value);
			if (search.case === 'mediaEnd') this.finishEventMedia(search.value);
			if (search.case === 'error') this.failEventSearch(search.value);
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
		if (stored.case === 'keyFrame') {
			this.#playbacks.get(stored.value.storedMediaId)?.receiveKeyFrame(stored.value);
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

	private receiveEventSearchResult(result: import('./proto/webrtc_pb').EventSearchResult): void {
		const pending = this.#eventSearchPending.get(result.queryId);
		if (!pending || !result.hit) return;
		const sequence = numeric(result.sequence);
		if (pending.sequences.has(sequence)) {
			this.failEventSearchPending(result.queryId, 'Event search repeated a result sequence.');
			return;
		}
		pending.sequences.add(sequence);
		pending.hits.push(result.hit);
	}

	private finishEventSearch(end: import('./proto/webrtc_pb').EventSearchQueryEnd): void {
		const pending = this.#eventSearchPending.get(end.queryId);
		if (!pending) return;
		this.#eventSearchPending.delete(end.queryId);
		clearTimeout(pending.timeout);
		if (pending.hits.length !== numeric(end.resultCount)) {
			pending.reject(new Error('Event search delivery was incomplete.'));
			return;
		}
		try {
			pending.resolve({
				hits: pending.hits.map(eventPreviewHit),
				nextPageToken: end.nextPageToken,
				candidatesTruncated: end.candidatesTruncated
			});
		} catch (cause) {
			pending.reject(
				cause instanceof Error ? cause : new Error('Event search result metadata was invalid.')
			);
		}
	}

	private receiveEventMediaChunk(chunk: EventSearchMediaChunk): void {
		const pending = this.#eventMediaPending.get(chunk.transferId);
		if (!pending || !pending.objectIds.has(chunk.objectId)) return;
		const byteLength = numeric(chunk.byteLen);
		if (
			chunk.representation !== StoredMediaObjectRepresentation.ENCODED_KEYFRAME ||
			chunk.chunkCount === 0 ||
			chunk.chunkIndex >= chunk.chunkCount ||
			byteLength <= 0 ||
			byteLength > maxEventKeyframeBytes
		) {
			this.failEventMediaPending(
				chunk.transferId,
				'Event keyframe chunk was invalid or oversized.'
			);
			return;
		}
		const accumulator = pending.objects.get(chunk.objectId) ?? {
			chunkCount: chunk.chunkCount,
			chunks: Array.from<Uint8Array | undefined>({ length: chunk.chunkCount }),
			byteLength,
			receivedBytes: 0,
			contentType: chunk.contentType,
			codec: chunk.codec,
			width: chunk.width,
			height: chunk.height,
			decoderConfig: chunk.decoderConfig,
			nalLengthSize: chunk.nalLengthSize
		};
		if (
			accumulator.chunkCount !== chunk.chunkCount ||
			accumulator.byteLength !== byteLength ||
			accumulator.contentType !== chunk.contentType ||
			accumulator.codec !== chunk.codec ||
			accumulator.width !== chunk.width ||
			accumulator.height !== chunk.height ||
			accumulator.nalLengthSize !== chunk.nalLengthSize ||
			!bytesEqual(accumulator.decoderConfig, chunk.decoderConfig) ||
			accumulator.chunks[chunk.chunkIndex] !== undefined ||
			accumulator.receivedBytes + chunk.payload.byteLength > accumulator.byteLength
		) {
			this.failEventMediaPending(chunk.transferId, 'Event keyframe chunks were inconsistent.');
			return;
		}
		accumulator.chunks[chunk.chunkIndex] = chunk.payload;
		accumulator.receivedBytes += chunk.payload.byteLength;
		pending.objects.set(chunk.objectId, accumulator);
	}

	private finishEventMedia(end: import('./proto/webrtc_pb').EventSearchMediaEnd): void {
		const pending = this.#eventMediaPending.get(end.transferId);
		if (!pending) return;
		this.#eventMediaPending.delete(end.transferId);
		clearTimeout(pending.timeout);
		if (
			end.objectCount !== pending.objectIds.size ||
			pending.objects.size !== pending.objectIds.size
		) {
			pending.reject(new Error('Event keyframe delivery was incomplete.'));
			return;
		}
		const objects = new Map<string, EncodedEventKeyframe>();
		for (const objectId of pending.objectIds) {
			const object = pending.objects.get(objectId);
			if (
				!object ||
				object.receivedBytes !== object.byteLength ||
				!object.chunks.every((chunk) => chunk !== undefined)
			) {
				pending.reject(new Error('Event keyframe delivery was incomplete.'));
				return;
			}
			objects.set(objectId, {
				contentType: object.contentType,
				codec: object.codec,
				width: object.width,
				height: object.height,
				decoderConfig: object.decoderConfig,
				nalLengthSize: object.nalLengthSize,
				payload: concatenateChunks(object.chunks)
			});
		}
		pending.resolve(objects);
	}

	private failEventSearch(error: import('./proto/webrtc_pb').EventSearchError): void {
		if (error.context.case === 'queryId') {
			this.failEventSearchPending(error.context.value, error.message || 'Event search failed.');
		}
		if (error.context.case === 'transferId') {
			this.failEventMediaPending(
				error.context.value,
				error.message || 'Event keyframe transfer failed.'
			);
		}
	}

	private failEventSearchPending(queryId: string, message: string): void {
		const pending = this.#eventSearchPending.get(queryId);
		if (!pending) return;
		this.#eventSearchPending.delete(queryId);
		clearTimeout(pending.timeout);
		pending.reject(new Error(message));
	}

	private failEventMediaPending(transferId: string, message: string): void {
		const pending = this.#eventMediaPending.get(transferId);
		if (!pending) return;
		this.#eventMediaPending.delete(transferId);
		clearTimeout(pending.timeout);
		pending.reject(new Error(message));
	}

	private receiveTimelinePage(page: import('./proto/webrtc_pb').StoredMediaQueryPage): void {
		const pending = this.#timelinePending.get(page.queryId);
		if (!pending) return;
		pending.pages.add(numeric(page.sequence));
		const ranges: StoredTimelineRange[] = [];
		for (const range of page.availability) {
			if (!range.startTime || !range.endTime) continue;
			ranges.push({
				sourceId: range.sourceId,
				streamId: range.streamId,
				startMs: timestampDate(range.startTime).getTime(),
				endMs: timestampDate(range.endTime).getTime()
			});
		}
		pending.ranges.push(...ranges);
		pending.events.push(...page.events);
		pending.onPage?.({
			ranges,
			events: page.events.map((event) => recordingEvent(event, new Map(), () => undefined))
		});
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
			contentType: attachment.contentType,
			byteCount: 0
		};
		const inFlightBytes = [...pending.attachments.values()].reduce(
			(total, value) => total + value.byteCount,
			0
		);
		if (
			accumulator.chunkCount !== chunkCount ||
			accumulator.contentType !== attachment.contentType ||
			accumulator.chunks[chunkIndex] !== undefined ||
			accumulator.byteCount + attachment.payload.byteLength > maxTimelineAttachmentBytes ||
			inFlightBytes + attachment.payload.byteLength > maxTimelineInFlightAttachmentBytes
		) {
			pending.reject(new Error('Stored event attachment chunks were invalid or oversized.'));
			this.#timelinePending.delete(attachment.context.value);
			clearTimeout(pending.timeout);
			return;
		}
		accumulator.chunks[chunkIndex] = attachment.payload;
		accumulator.byteCount += attachment.payload.byteLength;
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
		for (const queryId of this.#eventSearchPending.keys()) {
			this.failEventSearchPending(queryId, message);
		}
		for (const transferId of this.#eventMediaPending.keys()) {
			this.failEventMediaPending(transferId, message);
		}
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
		return this.releaseTransport(true);
	}

	private releaseTransport(revokeObjectUrls: boolean): string | null {
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
		if (revokeObjectUrls) {
			for (const url of this.#objectUrls) URL.revokeObjectURL(url);
			this.#objectUrls.clear();
		}
		this.publishCapabilities([]);
		this.failPending('WebRTC control connection closed.');
		return sessionId;
	}
}

type CompletedStoredObject = {
	generation: bigint;
	payload: Uint8Array;
	deliveredThroughMs?: number;
	sourceBufferContentType?: string;
};

type StoredChunkAccumulator = ChunkAccumulator & {
	generation: bigint;
	deliveredThroughMs?: number;
	sourceBufferContentType?: string;
};

type StoredKeyFrameAccumulator = {
	generation: bigint;
	frameId: bigint;
	configurationRevision: bigint;
	chunkCount: number;
	chunks: Array<Uint8Array | undefined>;
	byteCount: number;
	timestampMs: number;
	codec: string;
	width: number;
	height: number;
	decoderConfig: Uint8Array;
};

type PendingStoredSeek = {
	timestampMs: number;
	resolve: () => void;
	reject: (error: Error) => void;
};

export class StoredMediaPlayback {
	readonly id: string;
	readonly sourceId: string;
	readonly streamId: 'main' | 'sub';
	url: string;
	anchorTimeMs = 0;
	initialOffsetSeconds = 0;
	error: string | null = null;

	#mediaSource: MediaSource;
	#objectUrls = new Set<string>();
	#sourceBuffer: SourceBuffer | null = null;
	#contentType: string | null = null;
	#sourceBufferContentType: string | null = null;
	#generation = 0n;
	#chunks = new Map<string, StoredChunkAccumulator>();
	#completed: CompletedStoredObject[] = [];
	#appendQueue: Array<{ payload: Uint8Array; sourceBufferContentType?: string }> = [];
	#seek: (timestampMs: number) => Promise<StoredMediaState>;
	#refill: (timestampMs: number) => Promise<StoredMediaState>;
	#updatePlayback: (
		playing: boolean | undefined,
		playbackRate: number | undefined,
		mode: StoredMediaMode | undefined
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
	#mode = StoredMediaMode.PLAYBACK;
	#seekInFlight = false;
	#pendingSeek: PendingStoredSeek | null = null;
	#currentSeek: PendingStoredSeek | null = null;
	#seekTimer: ReturnType<typeof setTimeout> | null = null;
	#lastSeekDispatchMs = Number.NEGATIVE_INFINITY;
	#blockedGeneration: bigint | null = null;
	#keyFrameChunks = new Map<string, StoredKeyFrameAccumulator>();
	#keyFrameListeners = new Set<(preview: StoredMediaKeyFramePreview) => void>();
	#errorListeners = new Set<(message: string) => void>();
	#startupListeners = new Set<(event: StoredMediaStartupEvent) => void>();
	#startupEvents: StoredMediaStartupEvent[] = [];
	#latestKeyFrame: StoredMediaKeyFramePreview | null = null;
	#firstFragmentGenerations = new Set<bigint>();

	constructor(
		id: string,
		sourceId: string,
		streamId: 'main' | 'sub',
		seek: (timestampMs: number) => Promise<StoredMediaState>,
		refill: (timestampMs: number) => Promise<StoredMediaState>,
		updatePlayback: (
			playing: boolean | undefined,
			playbackRate: number | undefined,
			mode: StoredMediaMode | undefined
		) => Promise<StoredMediaState>,
		close: () => Promise<void>
	) {
		this.id = id;
		this.sourceId = sourceId;
		this.streamId = streamId;
		this.#seek = seek;
		this.#refill = refill;
		this.#updatePlayback = updatePlayback;
		this.#close = close;
		this.#mediaSource = new MediaSource();
		this.url = URL.createObjectURL(this.#mediaSource);
		this.#objectUrls.add(this.url);
		this.listenForSourceOpen();
	}

	get contentType(): string | null {
		return this.#contentType;
	}

	configure(state: StoredMediaState): void {
		if (!state.delivery || !state.requestedTime || !state.fragmentTime) {
			throw new Error('Server returned incomplete stored media state.');
		}
		if (this.#generation !== 0n && state.generation < this.#generation) return;
		if (this.#generation !== 0n && state.generation !== this.#generation) {
			this.replaceMediaSource();
		}
		this.#generation = state.generation;
		this.#mode = state.mode as StoredMediaMode;
		this.#contentType = state.delivery.contentType;
		this.reportStartup('metadata', state.delivery.contentType);
		this.anchorTimeMs = timestampDate(state.fragmentTime).getTime();
		this.#maxBufferMs = state.delivery.maxBufferDuration
			? protoDurationMs(state.delivery.maxBufferDuration)
			: 0;
		this.#endTimeMs = state.endTime ? timestampDate(state.endTime).getTime() : null;
		this.#ended = state.status === StoredMediaStatus.ENDED;
		this.#playing = state.playing;
		this.#playbackRate = state.playbackRate;
		this.#deliveredThroughMs = this.anchorTimeMs;
		this.initialOffsetSeconds = Math.max(
			0,
			(timestampDate(state.requestedTime).getTime() - this.anchorTimeMs) / 1_000
		);
		this.#completed = this.#completed.filter((object) => object.generation === this.#generation);
		for (const [key, chunks] of this.#chunks) {
			if (chunks.generation !== this.#generation) this.#chunks.delete(key);
		}
		for (const [key, chunks] of this.#keyFrameChunks) {
			if (chunks.generation !== this.#generation) this.#keyFrameChunks.delete(key);
		}
		if (this.#latestKeyFrame && this.#latestKeyFrame.generation < this.#generation) {
			this.#latestKeyFrame = null;
		}
		for (const object of this.#completed) {
			this.#appendQueue.push({
				payload: object.payload,
				sourceBufferContentType: object.sourceBufferContentType
			});
			if (object.deliveredThroughMs !== undefined) {
				this.#deliveredThroughMs = Math.max(this.#deliveredThroughMs, object.deliveredThroughMs);
			}
		}
		this.#completed = [];
		if (this.#mode === StoredMediaMode.PLAYBACK) this.initializeSourceBuffer();
		if (this.#endTimeMs !== null && this.#mediaSource.readyState === 'open') {
			this.#mediaSource.duration = Math.max(0, (this.#endTimeMs - this.anchorTimeMs) / 1_000);
		}
		this.flushAppendQueue();
		this.finishIfEnded();
	}

	seek(timestampMs: number): Promise<void> {
		if (this.#closed) return Promise.reject(new Error('Stored media playback is closed.'));
		if (!Number.isFinite(timestampMs)) {
			return Promise.reject(new Error('Stored media seek timestamp is invalid.'));
		}
		return new Promise((resolve, reject) => {
			this.#pendingSeek?.resolve();
			this.#pendingSeek = { timestampMs, resolve, reject };
			emitTimelinePerformanceEvent('ScrubSeekQueued', {
				sourceId: this.sourceId,
				cursorId: this.id,
				targetMs: timestampMs
			});
			this.scheduleSeek();
		});
	}

	canSeekLocally(timestampMs: number): boolean {
		return timestampMs >= this.anchorTimeMs && timestampMs < this.#deliveredThroughMs;
	}

	onKeyFrame(listener: (preview: StoredMediaKeyFramePreview) => void): () => void {
		this.#keyFrameListeners.add(listener);
		if (this.#latestKeyFrame) listener(this.#latestKeyFrame);
		return () => this.#keyFrameListeners.delete(listener);
	}

	onError(listener: (message: string) => void): () => void {
		this.#errorListeners.add(listener);
		if (this.error) listener(this.error);
		return () => this.#errorListeners.delete(listener);
	}

	onStartup(listener: (event: StoredMediaStartupEvent) => void): () => void {
		this.#startupListeners.add(listener);
		for (const event of this.#startupEvents) listener(event);
		return () => this.#startupListeners.delete(listener);
	}

	async enterScrub(): Promise<void> {
		if (this.#closed) throw new Error('Stored media playback is closed.');
		if (this.#mode === StoredMediaMode.SCRUB && !this.#playing) return;
		const state = await this.#updatePlayback(false, undefined, StoredMediaMode.SCRUB);
		this.acceptPlaybackState(state);
	}

	async commitPlayback(playing: boolean, playbackRate: number): Promise<void> {
		if (this.#closed) throw new Error('Stored media playback is closed.');
		const state = await this.#updatePlayback(playing, playbackRate, StoredMediaMode.PLAYBACK);
		this.acceptPlaybackState(state);
		if (playing) this.observe(this.initialOffsetSeconds);
	}

	receiveInitialization(initialization: StoredMediaInitialization): void {
		this.receiveChunks(
			`init:${initialization.generation}:${initialization.initializationId}`,
			initialization.generation,
			initialization.chunkIndex,
			initialization.chunkCount,
			initialization.contentType,
			initialization.payload,
			undefined,
			initialization.contentType
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

	receiveKeyFrame(keyFrame: StoredMediaKeyFrame): void {
		const configuration = keyFrame.configuration;
		const frame = keyFrame.frame;
		const format = configuration?.format?.format;
		if (
			keyFrame.storedMediaId !== this.id ||
			!configuration ||
			!frame ||
			format?.case !== 'video' ||
			!configuration.codec?.name ||
			!frame.timestamp ||
			!frame.keyFrame ||
			frame.streamBindingId !== configuration.streamBindingId ||
			frame.configurationRevision !== configuration.configurationRevision ||
			frame.fragmentCount === 0 ||
			frame.fragmentIndex >= frame.fragmentCount
		) {
			this.fail('Stored media keyframe metadata was invalid.');
			return;
		}
		if (keyFrame.generation < this.#generation) return;
		if (this.#generation !== 0n && keyFrame.generation > this.#generation && !this.#seekInFlight) {
			return;
		}
		const timestampMs = timestampDate(frame.timestamp).getTime();
		const key = `${keyFrame.generation}:${frame.frameId}`;
		const accumulator = this.#keyFrameChunks.get(key) ?? {
			generation: keyFrame.generation,
			frameId: frame.frameId,
			configurationRevision: configuration.configurationRevision,
			chunkCount: frame.fragmentCount,
			chunks: Array.from<Uint8Array | undefined>({ length: frame.fragmentCount }),
			byteCount: 0,
			timestampMs,
			codec: configuration.codec.name,
			width: format.value.width,
			height: format.value.height,
			decoderConfig: format.value.decoderConfig
		};
		if (
			accumulator.generation !== keyFrame.generation ||
			accumulator.frameId !== frame.frameId ||
			accumulator.configurationRevision !== configuration.configurationRevision ||
			accumulator.chunkCount !== frame.fragmentCount ||
			accumulator.timestampMs !== timestampMs ||
			accumulator.codec !== configuration.codec.name ||
			accumulator.width !== format.value.width ||
			accumulator.height !== format.value.height ||
			!bytesEqual(accumulator.decoderConfig, format.value.decoderConfig) ||
			accumulator.chunks[frame.fragmentIndex] !== undefined ||
			accumulator.byteCount + frame.payload.byteLength > maxEventKeyframeBytes
		) {
			this.fail('Stored media keyframe chunks were inconsistent or oversized.');
			return;
		}
		accumulator.chunks[frame.fragmentIndex] = frame.payload;
		accumulator.byteCount += frame.payload.byteLength;
		this.#keyFrameChunks.set(key, accumulator);
		if (!accumulator.chunks.every((chunk) => chunk !== undefined)) return;
		this.#keyFrameChunks.delete(key);
		const preview: StoredMediaKeyFramePreview = {
			storedMediaId: this.id,
			generation: keyFrame.generation,
			timestampMs,
			configurationRevision: configuration.configurationRevision,
			contentType: accumulator.codec.startsWith('avc1') ? 'video/avc' : 'video/hevc',
			codec: accumulator.codec,
			width: accumulator.width,
			height: accumulator.height,
			decoderConfig: accumulator.decoderConfig,
			nalLengthSize: 0,
			payload: concatenateChunks(accumulator.chunks)
		};
		this.#latestKeyFrame = preview;
		for (const listener of this.#keyFrameListeners) listener(preview);
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
		const lowWatermarkMs = Math.min(1_500, this.#maxBufferMs / 2);
		if (this.#deliveredThroughMs - playbackTimeMs > lowWatermarkMs) return;
		this.#refillInFlight = true;
		emitTimelinePerformanceEvent('ReplayRefill', {
			sourceId: this.sourceId,
			cursorId: this.id,
			generation: String(this.#generation),
			playbackTimeMs
		});
		void this.#refill(playbackTimeMs)
			.then((state) => this.acceptPlaybackState(state))
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
		void this.#updatePlayback(playing, undefined, undefined)
			.then((state) => {
				this.acceptPlaybackState(state);
				if (playing) this.observe(this.initialOffsetSeconds);
			})
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
		void this.#updatePlayback(undefined, playbackRate, undefined)
			.then((state) => this.acceptPlaybackState(state))
			.catch((error) => {
				if (this.#playbackRate === playbackRate) this.#playbackRate = previous;
				this.fail(error instanceof Error ? error.message : 'Unable to update stored playback.');
			});
	}

	private acceptPlaybackState(state: StoredMediaState): void {
		if (
			state.generation !== this.#generation ||
			state.mode !== this.#mode ||
			state.delivery?.contentType !== this.#contentType
		) {
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
		if (this.error === message) return;
		this.error = message;
		for (const listener of this.#errorListeners) listener(message);
	}

	dispose(): void {
		if (this.#closed) return;
		this.#closed = true;
		this.#chunks.clear();
		this.#keyFrameChunks.clear();
		this.#keyFrameListeners.clear();
		this.#errorListeners.clear();
		this.#startupListeners.clear();
		this.#startupEvents = [];
		this.#latestKeyFrame = null;
		this.#firstFragmentGenerations.clear();
		this.#completed = [];
		this.#appendQueue = [];
		this.#refillInFlight = false;
		if (this.#seekTimer) clearTimeout(this.#seekTimer);
		this.#seekTimer = null;
		const closedError = new Error('Stored media playback is closed.');
		this.#pendingSeek?.reject(closedError);
		this.#currentSeek?.reject(closedError);
		this.#pendingSeek = null;
		this.#currentSeek = null;
		for (const url of this.#objectUrls) URL.revokeObjectURL(url);
		this.#objectUrls.clear();
	}

	private receiveChunks(
		key: string,
		generation: bigint,
		chunkIndex: number,
		chunkCount: number,
		contentType: string,
		payload: Uint8Array,
		deliveredThroughMs: number | undefined,
		sourceBufferContentType?: string
	): void {
		if (this.#closed || chunkCount === 0 || chunkIndex >= chunkCount) return;
		const accumulator = this.#chunks.get(key) ?? {
			generation,
			chunkCount,
			chunks: Array.from<Uint8Array | undefined>({ length: chunkCount }),
			contentType,
			deliveredThroughMs,
			sourceBufferContentType
		};
		if (
			accumulator.generation !== generation ||
			accumulator.chunkCount !== chunkCount ||
			accumulator.contentType !== contentType ||
			accumulator.deliveredThroughMs !== deliveredThroughMs ||
			accumulator.sourceBufferContentType !== sourceBufferContentType ||
			accumulator.chunks[chunkIndex] !== undefined
		) {
			this.fail('Stored media chunks were inconsistent.');
			return;
		}
		accumulator.chunks[chunkIndex] = payload;
		this.#chunks.set(key, accumulator);
		if (!accumulator.chunks.every((chunk) => chunk !== undefined)) return;
		this.#chunks.delete(key);
		if (accumulator.sourceBufferContentType) {
			this.reportStartup('initialization', accumulator.sourceBufferContentType);
		} else if (accumulator.deliveredThroughMs !== undefined) {
			this.reportStartup('first-fragment', accumulator.contentType);
		}
		const completed = {
			generation,
			payload: concatenateChunks(accumulator.chunks),
			deliveredThroughMs: accumulator.deliveredThroughMs,
			sourceBufferContentType: accumulator.sourceBufferContentType
		};
		if (this.#generation === 0n) {
			this.#completed.push(completed);
			return;
		}
		if (generation > this.#generation) {
			if (this.#seekInFlight) this.#completed.push(completed);
			return;
		}
		if (generation !== this.#generation || generation === this.#blockedGeneration) return;
		if (accumulator.deliveredThroughMs !== undefined) {
			this.#deliveredThroughMs = Math.max(this.#deliveredThroughMs, accumulator.deliveredThroughMs);
		}
		this.#appendQueue.push({
			payload: completed.payload,
			sourceBufferContentType: completed.sourceBufferContentType
		});
		if (
			accumulator.deliveredThroughMs !== undefined &&
			!this.#firstFragmentGenerations.has(generation)
		) {
			this.#firstFragmentGenerations.add(generation);
			emitTimelinePerformanceEvent('ReplayFirstFragment', {
				sourceId: this.sourceId,
				cursorId: this.id,
				generation: String(generation)
			});
		}
		this.flushAppendQueue();
	}

	private scheduleSeek(): void {
		if (this.#seekInFlight || this.#seekTimer || !this.#pendingSeek || this.#closed) return;
		const delayMs = Math.max(0, 50 - (performance.now() - this.#lastSeekDispatchMs));
		this.#seekTimer = setTimeout(() => {
			this.#seekTimer = null;
			this.dispatchSeek();
		}, delayMs);
	}

	private dispatchSeek(): void {
		const seek = this.#pendingSeek;
		if (!seek || this.#seekInFlight || this.#closed) return;
		this.#pendingSeek = null;
		this.#currentSeek = seek;
		this.#seekInFlight = true;
		this.#blockedGeneration = this.#generation;
		this.#lastSeekDispatchMs = performance.now();
		emitTimelinePerformanceEvent('ScrubSeekSent', {
			sourceId: this.sourceId,
			cursorId: this.id,
			generation: String(this.#generation),
			targetMs: seek.timestampMs
		});
		void this.#seek(seek.timestampMs)
			.then((state) => {
				if (this.#closed) return;
				this.configure(state);
				seek.resolve();
			})
			.catch((cause) => {
				seek.reject(cause instanceof Error ? cause : new Error('Stored media seek failed.'));
			})
			.finally(() => {
				if (this.#currentSeek === seek) this.#currentSeek = null;
				this.#seekInFlight = false;
				this.#blockedGeneration = null;
				this.scheduleSeek();
			});
	}

	private replaceMediaSource(): void {
		this.#sourceBuffer = null;
		this.#sourceBufferContentType = null;
		this.#appendQueue = [];
		this.#refillInFlight = false;
		this.#mediaSource = new MediaSource();
		this.url = URL.createObjectURL(this.#mediaSource);
		this.#objectUrls.add(this.url);
		this.listenForSourceOpen();
	}

	private reportStartup(phase: StoredMediaStartupPhase, contentType: string): void {
		if (
			this.#startupEvents.some(
				(event) => event.generation === this.#generation && event.phase === phase
			)
		) {
			return;
		}
		const event = { phase, generation: this.#generation, contentType };
		this.#startupEvents.push(event);
		for (const listener of this.#startupListeners) listener(event);
	}

	private listenForSourceOpen(): void {
		const mediaSource = this.#mediaSource;
		mediaSource.addEventListener(
			'sourceopen',
			() => {
				if (this.#mediaSource === mediaSource) this.initializeSourceBuffer();
			},
			{ once: true }
		);
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
			const sourceBuffer = this.#mediaSource.addSourceBuffer(this.#contentType);
			this.#sourceBuffer = sourceBuffer;
			this.#sourceBufferContentType = this.#contentType;
			sourceBuffer.mode = 'sequence';
			sourceBuffer.addEventListener('updateend', () => {
				if (this.#sourceBuffer !== sourceBuffer) return;
				this.flushAppendQueue();
				this.finishIfEnded();
			});
			sourceBuffer.addEventListener('error', () => {
				if (this.#sourceBuffer === sourceBuffer) this.fail('Browser rejected stored media bytes.');
			});
			this.flushAppendQueue();
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
			this.observe(this.initialOffsetSeconds);
			this.finishIfEnded();
			return;
		}
		const next = this.#appendQueue.shift();
		if (!next) return;
		try {
			if (
				next.sourceBufferContentType &&
				next.sourceBufferContentType !== this.#sourceBufferContentType
			) {
				if (!MediaSource.isTypeSupported(next.sourceBufferContentType)) {
					this.fail(`Browser does not support ${next.sourceBufferContentType}.`);
					return;
				}
				if (typeof sourceBuffer.changeType !== 'function') {
					this.fail('Browser cannot switch stored media codecs.');
					return;
				}
				sourceBuffer.changeType(next.sourceBufferContentType);
				this.#sourceBufferContentType = next.sourceBufferContentType;
			}
			sourceBuffer.appendBuffer(ownedArrayBuffer(next.payload));
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

function recordingsForSources(
	sourceIds: readonly string[],
	date: string,
	ranges: StoredTimelineRange[]
): RecordingsResponse[] {
	return sourceIds.map((cameraId) => {
		const segments = timelineSegments(cameraId, date, ranges);
		return {
			camera_id: cameraId,
			date,
			dates: segments.length > 0 ? [date] : [],
			segments
		};
	});
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
	const thumbnailBlob = attachment?.chunks.every((chunk) => chunk !== undefined)
		? new Blob([ownedArrayBuffer(concatenateChunks(attachment.chunks))], {
				type: attachment.contentType
			})
		: undefined;
	const thumbnailUrl = thumbnailBlob ? URL.createObjectURL(thumbnailBlob) : null;
	if (thumbnailUrl) onObjectUrl(thumbnailUrl);
	return {
		id: event.eventId,
		source_id: event.sourceId,
		revision: numeric(event.revision),
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
		thumbnail_url: thumbnailUrl,
		thumbnail_blob: thumbnailBlob,
		attachments: event.attachments.map((attachment) => ({
			id: attachment.attachmentId,
			type: attachment.attachmentType,
			content_type: attachment.contentType,
			byte_length: attachment.byteLen === undefined ? null : numeric(attachment.byteLen),
			ordinal: attachment.ordinal,
			timestamp_ms: attachment.timestamp ? timestampDate(attachment.timestamp).getTime() : null
		}))
	};
}

function eventPreviewHit(hit: ProtoEventSearchHit): EventPreviewHit {
	if (!hit.startTime || !hit.previewStartTime || !hit.previewEndTime) {
		throw new Error('Event search result omitted required timestamps.');
	}
	const origin =
		hit.origin === EventOrigin.CAMERA
			? 'camera'
			: hit.origin === EventOrigin.KEEPPEEK
				? 'keeppeek'
				: null;
	if (origin === null) throw new Error('Event search result omitted its origin.');
	return {
		eventId: hit.eventId,
		sourceId: hit.sourceId,
		eventType: hit.eventType,
		origin,
		startMs: timestampDate(hit.startTime).getTime(),
		endMs: hit.endTime ? timestampDate(hit.endTime).getTime() : null,
		confidence: hit.confidence ?? null,
		bbox: hit.boundingBox
			? [hit.boundingBox.x, hit.boundingBox.y, hit.boundingBox.width, hit.boundingBox.height]
			: null,
		zone: hit.zone ?? null,
		text: hit.text ?? null,
		hasImageAttachment: hit.hasImageAttachment,
		previewStartMs: timestampDate(hit.previewStartTime).getTime(),
		previewEndMs: timestampDate(hit.previewEndTime).getTime(),
		keyframes: hit.keyframes.flatMap((keyframe) =>
			keyframe.eventTime && keyframe.fragmentStartTime
				? [
						{
							sourceId: keyframe.sourceId,
							streamId: keyframe.streamId,
							recordingId: keyframe.recordingId,
							fragmentSequence: keyframe.fragmentSequence,
							eventTimeMs: timestampDate(keyframe.eventTime).getTime(),
							fragmentStartMs: timestampDate(keyframe.fragmentStartTime).getTime(),
							byteLength: numeric(keyframe.byteLen)
						}
					]
				: []
		),
		keyframesTruncated: hit.keyframesTruncated
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

function bytesEqual(left: Uint8Array, right: Uint8Array): boolean {
	return left.byteLength === right.byteLength && left.every((byte, index) => byte === right[index]);
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

function notificationRuleRecord(record: ProtoNotificationRuleRecord): NotificationRuleRecord {
	return {
		id: record.ruleId,
		ownerId: record.ownerId,
		active: record.activeDefinitionJson
			? parseNotificationRuleDefinition(record.activeDefinitionJson)
			: null,
		activeRevision: record.activeRevision,
		draft: parseNotificationRuleDefinition(record.draftDefinitionJson),
		draftRevision: record.draftRevision,
		createdAtMs: Number(record.createdAtMs),
		updatedAtMs: Number(record.updatedAtMs),
		lastMatchAtMs: record.lastMatchAtMs === undefined ? null : Number(record.lastMatchAtMs),
		lastDeliveryAtMs: record.lastDeliveryAtMs === undefined ? null : Number(record.lastDeliveryAtMs)
	};
}

function notificationInbox(inbox: ProtoNotificationInbox): NotificationInbox {
	return {
		items: inbox.items.map(notificationItem),
		unreadCount: inbox.unreadCount
	};
}

function notificationItem(item: ProtoNotificationItem): NotificationItem {
	return {
		logicalId: item.logicalId,
		ruleId: item.ruleId,
		sourceId: item.sourceId,
		lifecycle: item.lifecycle,
		stage: notificationStage(item.stage),
		revision: item.revision,
		title: item.title,
		body: item.body,
		deepLink: item.deepLink,
		attachmentAvailable: item.attachmentAvailable,
		severity: notificationSeverity(item.severity),
		createdAtMs: Number(item.createdAtMs),
		updatedAtMs: Number(item.updatedAtMs),
		seenAtMs: item.seenAtMs === undefined ? null : Number(item.seenAtMs),
		acknowledgedAtMs: item.acknowledgedAtMs === undefined ? null : Number(item.acknowledgedAtMs)
	};
}

function notificationHistoryGroup(group: ProtoNotificationHistoryGroup): NotificationHistoryGroup {
	if (!group.notification) {
		throw new Error('Server returned notification history without its logical notification.');
	}
	return {
		notification: notificationItem(group.notification),
		events: group.events.map(notificationHistoryEvent),
		attempts: group.attempts.map(notificationDeliveryAttempt)
	};
}

function notificationHistoryEvent(event: ProtoNotificationHistoryEvent): NotificationHistoryEvent {
	return {
		sequence: event.sequence,
		revision: event.revision,
		stage: notificationStage(event.stage),
		outcome: event.outcome,
		reason: event.reason ?? null,
		occurredAtMs: Number(event.occurredAtMs),
		nextEligibleAtMs: event.nextEligibleAtMs === undefined ? null : Number(event.nextEligibleAtMs)
	};
}

function notificationDeliveryAttempt(
	attempt: ProtoNotificationDeliveryAttempt
): NotificationDeliveryAttempt {
	return {
		sequence: attempt.sequence,
		channel: notificationChannel(attempt.channel),
		stage: notificationStage(attempt.stage),
		attempt: attempt.attempt,
		outcome: attempt.outcome,
		targetHash: attempt.targetHash,
		providerStatus: attempt.providerStatus ?? null,
		providerRequestId: attempt.providerRequestId ?? null,
		providerAcknowledgedAtMs:
			attempt.providerAcknowledgedAtMs === undefined
				? null
				: Number(attempt.providerAcknowledgedAtMs),
		providerExpiredAtMs:
			attempt.providerExpiredAtMs === undefined ? null : Number(attempt.providerExpiredAtMs),
		providerAcknowledgedByHash: attempt.providerAcknowledgedByHash ?? null,
		providerAcknowledgementState: providerAcknowledgementState(
			attempt.providerAcknowledgementState
		),
		reason: attempt.reason ?? null,
		attemptedAtMs: Number(attempt.attemptedAtMs),
		retryAtMs: attempt.retryAtMs === undefined ? null : Number(attempt.retryAtMs)
	};
}

function providerAcknowledgementState(
	value: string | undefined
): 'pending' | 'acknowledged' | 'expired' | 'failed' | null {
	if (value === undefined || value === '') return null;
	if (value === 'pending' || value === 'acknowledged' || value === 'expired' || value === 'failed')
		return value;
	throw new Error(`Server returned unsupported provider acknowledgement state '${value}'.`);
}

function notificationStage(value: string): NotificationStage {
	if (value === 'preliminary' || value === 'enriched' || value === 'recovery') return value;
	throw new Error(`Server returned unsupported notification stage '${value}'.`);
}

function notificationSeverity(value: string): NotificationSeverity {
	if (value === 'info' || value === 'warning' || value === 'critical') return value;
	throw new Error(`Server returned unsupported notification severity '${value}'.`);
}

function notificationChannel(value: string): NotificationChannel {
	if (value === 'browser' || value === 'push' || value === 'webhook' || value === 'forwarder') {
		return value;
	}
	throw new Error(`Server returned unsupported notification channel '${value}'.`);
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

function protoCameraRecordingMode(mode: CameraRecordingMode): ProtoCameraRecordingMode {
	if (mode === 'off') return ProtoCameraRecordingMode.OFF;
	if (mode === 'sub') return ProtoCameraRecordingMode.SUB;
	if (mode === 'main') return ProtoCameraRecordingMode.MAIN;
	if (mode === 'both') return ProtoCameraRecordingMode.BOTH;
	return ProtoCameraRecordingMode.EVENT_BOOST;
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
		record_generic_motion_events: camera.recordGenericMotionEvents,
		recording_mode:
			camera.recordingMode === ProtoCameraRecordingMode.OFF
				? 'off'
				: camera.recordingMode === ProtoCameraRecordingMode.SUB
					? 'sub'
					: camera.recordingMode === ProtoCameraRecordingMode.MAIN
						? 'main'
						: camera.recordingMode === ProtoCameraRecordingMode.BOTH
							? 'both'
							: 'event-boost',
		event_recording_duration_secs: camera.eventRecordingDurationSecs || 60,
		health: (camera.health ?? null) as CameraSettings['health'],
		model: camera.model ?? null
	};
}

function cameraCatalogInfo(
	catalog: import('./proto/webrtc_pb').CameraCatalogInfo
): CameraCatalogInfo {
	return {
		version: catalog.version,
		tag: catalog.tag,
		generated_at: catalog.generatedAt,
		camera_count: catalog.cameraCount,
		website_url: catalog.websiteUrl
	};
}

function discoveredCameras(
	result: import('./proto/webrtc_pb').CameraDiscoveryResult
): DiscoveredCameraSettings[] {
	return result.cameras.map((camera) => ({
		ip: camera.ip,
		brand: camera.brand,
		name: camera.name ?? null,
		model: camera.model ?? null,
		onvif_port: camera.onvifPort ?? null,
		sources: camera.sources,
		configured: camera.configured,
		health: (camera.health ?? null) as DiscoveredCameraSettings['health'],
		catalog: camera.catalog ? cameraCatalogCamera(camera.catalog) : null
	}));
}

function cameraCatalogCamera(
	camera: import('./proto/webrtc_pb').CameraCatalogCamera
): CameraCatalogCamera {
	return {
		id: camera.id,
		brand: camera.brand,
		model: camera.model,
		aliases: [...camera.aliases],
		camera_type: camera.cameraType,
		resolution_label: camera.resolutionLabel ?? null,
		megapixels: camera.megapixels ?? null,
		sensor: camera.sensor ?? null,
		field_of_view: camera.fieldOfView ?? null,
		night_vision: camera.nightVision ?? null,
		ip_rating: camera.ipRating ?? null,
		ik_rating: camera.ikRating ?? null,
		two_way_audio: camera.twoWayAudio ?? null,
		release_year: camera.releaseYear ?? null,
		community_notes_count: camera.communityNotesCount,
		protocols: [...camera.protocols],
		codecs: [...camera.codecs],
		streams: camera.streams.map((stream) => ({
			name: stream.name,
			resolution: stream.resolution ?? null,
			fps: stream.fps ?? null,
			codec: stream.codec ?? null
		})),
		sources: [...camera.sources],
		stream_hints: camera.streamHints
			? {
					main_rtsp_url: camera.streamHints.mainRtspUrl ?? null,
					sub_rtsp_url: camera.streamHints.subRtspUrl ?? null
				}
			: null
	};
}

function runtimeConfiguration(config: SanitizedRuntimeConfiguration): SanitizedConfig {
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

function serverHealth(health: ServerHealthSnapshot): ServerHealthResponse {
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
			message: issue.message
		}))
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

function healthProfile(profile: HealthProfileSummary): ProfileSummary {
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

function canonicalCameraHealthState(value: string): CameraHealthState {
	if (
		value === 'starting' ||
		value === 'healthy' ||
		value === 'degraded' ||
		value === 'stale' ||
		value === 'reconnecting' ||
		value === 'offline' ||
		value === 'stopped' ||
		value === 'unknown'
	) {
		return value;
	}
	return 'unknown';
}

function canonicalCameraHealthReason(value: string): CameraHealthReason {
	if (
		value === 'healthy' ||
		value === 'starting' ||
		value === 'not_expected' ||
		value === 'battery_sleeping' ||
		value === 'evidence_unavailable' ||
		value === 'transport_disconnected' ||
		value === 'transport_reconnecting' ||
		value === 'transport_partially_connected' ||
		value === 'no_stream_report' ||
		value === 'stream_report_stale' ||
		value === 'frames_not_arriving' ||
		value === 'frames_below_expected' ||
		value === 'keyframes_missing' ||
		value === 'ingress_reconnects' ||
		value === 'ingress_drops' ||
		value === 'ingress_errors' ||
		value === 'recording_not_progressing'
	) {
		return value;
	}
	return 'unknown';
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
				const storedStream = stored?.streams.find((candidate) => candidate.streamId === stream);
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
					quality_rank: variant && variant.qualityRank > 0 ? variant.qualityRank : null,
					recorded_content_type: storedStream?.contentType ?? null,
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
