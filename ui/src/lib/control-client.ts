import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import { durationFromMs, timestampDate, timestampFromDate } from '@bufbuild/protobuf/wkt';
import {
	ApiRequestError,
	createSession,
	deleteSession,
	fetchLogSnapshot as fetchAuthenticatedLogSnapshot,
	fetchLogStream as fetchAuthenticatedLogStream,
	fetchMetricsSnapshot as fetchAuthenticatedMetricsSnapshot,
	fetchRecordingCoverage as fetchAuthenticatedRecordingCoverage
} from './api';
import type { MqttSettingsUpdate } from './integrations';
import { MqttControlClient } from './control-client-mqtt';
import type {
	AccessAuditEvent,
	AccessConnectionState,
	AccessCredential,
	AccessCredentialInput,
	AccessRole,
	AccessSession,
	IssuedAccessCredential
} from './access';
import { NotificationControlClient } from './control-client-notifications';
import { SystemControlClient, healthProfile, numeric } from './control-client-system';
import { emitTimelinePerformanceEvent } from './timeline-observability';
import {
	AccessRole as ProtoAccessRole,
	CameraBackend as ProtoCameraBackend,
	CameraConfigurationCommandSchema,
	CameraControlCommandSchema,
	CameraRecordingMode as ProtoCameraRecordingMode,
	CameraTransport as ProtoCameraTransport,
	CancelCameraDiscoverySchema,
	CancelEventSearchMediaSchema,
	CancelEventSearchQuerySchema,
	CancelExportJobSchema,
	CancelStoredMediaTimelineQuerySchema,
	CloseStoredMediaSchema,
	ControlEnvelopeSchema,
	CreateAccessCredentialSchema,
	CreateExportJobSchema,
	DataChannelKind,
	DiscoverCamerasSchema,
	DownloadExportSchema,
	EventAttachmentDescriptorSchema,
	EventExportSeedSchema,
	EventImageAvailability as ProtoEventImageAvailability,
	EventImageFilter as ProtoEventImageFilter,
	EventMetadataSearchSchema,
	EventOrigin,
	EventSearchCommandSchema,
	EventSearchField,
	EventSearchMediaObjectSchema,
	EventTextSearchSchema,
	ExportCommandSchema,
	ExportJobStatus,
	FetchEventSearchMediaSchema,
	GetAccessSessionSchema,
	GetCameraCatalogSchema,
	GetCameraConfigurationsSchema,
	GetCameraDiscoverySchema,
	GetCameraOnboardingDefaultsSchema,
	GetExportJobSchema,
	GetMotionDetectionSchema,
	ListAccessAuditSchema,
	ListAccessCredentialsSchema,
	ListAccessSessionsSchema,
	ListExportJobsSchema,
	MessageSchema,
	OpenStoredMediaSchema,
	OptionalStringUpdateSchema,
	OptionalUint32UpdateSchema,
	ProbeCameraStreamsSchema,
	PtzCommandSchema,
	PtzContinuousSchema,
	PtzPresetGotoSchema,
	PtzPresetListSchema,
	PtzStopSchema,
	QueryEventsSchema,
	QueryStoredMediaTimelineSchema,
	RefillStoredMediaSchema,
	RemoveCameraConfigurationSchema,
	RequestSchema,
	RetryExportJobSchema,
	RevokeAccessCredentialSchema,
	RevokeAccessSessionSchema,
	RotateAccessCredentialSchema,
	SearchCameraCatalogSchema,
	SeekStoredMediaSchema,
	ServerCommandSchema,
	SetAccessCredentialEnabledSchema,
	SetCameraManufacturerSchema,
	SetMotionDetectionSchema,
	SetStoredMediaPlaybackSchema,
	StoredMediaCommandSchema,
	StoredMediaEventQuerySchema,
	StoredMediaMode,
	StoredMediaObjectRepresentation,
	StoredMediaStatus,
	UpdateCameraConfigurationSchema,
	type AccessAuditEvent as ProtoAccessAuditEvent,
	type AccessCredential as ProtoAccessCredential,
	type AccessSession as ProtoAccessSession,
	type Event as ProtoEvent,
	type EventSearchHit as ProtoEventSearchHit,
	type EventSearchMediaObject as ProtoEventSearchMediaObject,
	type EventSearchMediaChunk,
	type ExportJob as ProtoExportJob,
	type MotionDetectionResult,
	type QueryEvents,
	type Request,
	type Response as ControlResponse,
	type ServerCapabilities,
	type StoredMediaFragment,
	type StoredMediaInitialization,
	type StoredMediaKeyFrame,
	type StoredMediaState,
	type EventAttachmentDescriptor as ProtoEventAttachmentDescriptor
} from './proto/webrtc_pb';
import { canonicalEventAttachment, eventIconKey } from './event-presentation';
import type {
	NotificationClearScope,
	NotificationHistoryGroup,
	NotificationInbox,
	NotificationRuleDefinition,
	NotificationRuleRecord,
	NotificationTestResult
} from './notifications';
import type {
	CameraBackend,
	CameraCatalogCamera,
	CameraCatalogInfo,
	CameraDetailsResponse,
	CameraListItem,
	CameraOnboardingDefaults,
	CameraRecordingMode,
	CameraSettings,
	CameraSettingsUpdate,
	CameraSettingsUpdateResponse,
	CameraStreamProbeResult,
	CameraTransport,
	DiscoveredCameraSettings,
	EventImageAvailability as EventImageAvailabilityState,
	LoggingSettings,
	MotionDetection,
	RecordingEvent,
	RecordingEventAttachment,
	RecordingEventsResponse,
	RecordingCoverageQuery,
	RecordingCoverageResponse,
	RecordingSegment,
	RecordingsResponse,
	SanitizedConfig,
	ServerHealthResponse,
	SettingsConfigUpdate,
	SettingsConfigUpdateResponse
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
type AccessStateListener = (state: AccessConnectionState) => void;

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
	eventSeed: MediaExportEventSeed | null;
};

export type MediaExportEventSeed = {
	eventId: string;
	revision: number;
	canonicalAttachment: RecordingEventAttachment | null;
	iconKey: RecordingEvent['icon_key'];
	imageAvailability: EventImageAvailabilityState;
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
	revision: number;
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
	canonicalAttachment: RecordingEventAttachment | null;
	attachments: RecordingEventAttachment[];
	imageAvailability: EventImageAvailabilityState;
	iconKey: RecordingEvent['icon_key'];
	rejectedIconKey: string | null;
	bboxAttachmentId: string | null;
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
	representation: StoredMediaObjectRepresentation;
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
	eventId: string;
	eventRevision: bigint;
	attachmentId: string;
};

type EventMediaResult =
	| { kind: 'keyframe'; media: EncodedEventKeyframe }
	| {
			kind: 'attachment';
			eventId: string;
			eventRevision: number;
			attachmentId: string;
			contentType: string;
			payload: Uint8Array;
	  };

type EventMediaPending = {
	objectIds: Set<string>;
	objects: Map<string, EventMediaAccumulator>;
	resolve: (objects: Map<string, EventMediaResult>) => void;
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
	#accessKey: string | null = null;
	#accessState: AccessConnectionState = {
		status: 'checking',
		session: null,
		message: null,
		generation: 0
	};
	#accessStateListeners = new Set<AccessStateListener>();
	#notifications = new NotificationControlClient((command) => this.request(command));
	#mqtt = new MqttControlClient((command) => this.request(command));
	#system = new SystemControlClient(
		(command) => this.request(command),
		(event) => recordingEvent(event, new Map<string, ChunkAccumulator>(), () => {})
	);

	onCapabilities(listener: CapabilityListener): () => void {
		this.#capabilityListeners.add(listener);
		listener(this.#capabilityIds);
		return () => this.#capabilityListeners.delete(listener);
	}

	onAccessState(listener: AccessStateListener): () => void {
		this.#accessStateListeners.add(listener);
		listener(this.#accessState);
		return () => this.#accessStateListeners.delete(listener);
	}

	async checkAccess(): Promise<void> {
		try {
			await this.getServerCapabilities();
		} catch (error) {
			if (this.#accessState.status === 'checking') {
				this.publishAccessState({
					status: 'error',
					session: null,
					message: error instanceof Error ? error.message : 'KeepPeek could not be reached.'
				});
			}
			throw error;
		}
	}

	async signIn(accessKey: string): Promise<void> {
		const candidate = accessKey.trim();
		if (candidate.length === 0 || candidate.length > 128) {
			throw new Error('Enter a valid access key.');
		}
		const previousAccessKey = this.#accessKey;
		const sessionId = this.release();
		if (sessionId !== null) {
			await deleteSession(sessionId, previousAccessKey).catch(() => undefined);
		}
		this.#accessKey = candidate;
		this.publishAccessState({ status: 'checking', session: null, message: null });
		await this.checkAccess();
	}

	async signOut(): Promise<void> {
		const accessKey = this.#accessKey;
		const sessionId = this.release();
		if (sessionId !== null) await deleteSession(sessionId, accessKey).catch(() => undefined);
		this.#accessKey = null;
		this.publishAccessState({
			status: 'sign-in-required',
			session: null,
			message: 'Signed out.'
		});
	}

	createWebRtcSession(offer: RTCSessionDescriptionInit) {
		return createSession(offer, this.#accessKey);
	}

	deleteWebRtcSession(sessionId: string, options: { keepalive?: boolean } = {}): Promise<void> {
		return deleteSession(sessionId, this.#accessKey, options);
	}

	async getLogSnapshot() {
		return this.authenticatedHttp(() => fetchAuthenticatedLogSnapshot(this.#accessKey));
	}

	async getMetricsSnapshot(): Promise<string> {
		return this.authenticatedHttp(() => fetchAuthenticatedMetricsSnapshot(this.#accessKey));
	}

	async getMqttIntegration() {
		return this.#mqtt.get();
	}

	async updateMqttIntegration(update: MqttSettingsUpdate) {
		return this.#mqtt.update(update);
	}

	async testMqttIntegration(update: MqttSettingsUpdate) {
		return this.#mqtt.test(update);
	}

	async getRecordingCoverage(
		query: RecordingCoverageQuery = {},
		signal?: AbortSignal
	): Promise<RecordingCoverageResponse> {
		return this.authenticatedHttp(
			() => fetchAuthenticatedRecordingCoverage(query, this.#accessKey, signal),
			[400, 409]
		);
	}

	async openLogStream(url: string, signal: AbortSignal): Promise<Response> {
		return this.authenticatedHttp(() => fetchAuthenticatedLogStream(url, this.#accessKey, signal));
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
		const objectId = `keyframe-${this.#nextStoredId++}`;
		const result = await this.fetchEventMediaObject(
			create(EventSearchMediaObjectSchema, {
				objectId,
				sourceId: keyframe.sourceId,
				streamId: keyframe.streamId,
				recordingId: keyframe.recordingId,
				fragmentSequence: keyframe.fragmentSequence,
				representation: StoredMediaObjectRepresentation.ENCODED_KEYFRAME
			}),
			signal
		);
		if (result.kind !== 'keyframe') {
			throw new Error('Event keyframe transfer returned an attachment.');
		}
		return result.media;
	}

	async fetchCanonicalEventAttachment(
		event: Pick<RecordingEvent, 'id' | 'source_id' | 'revision' | 'canonical_attachment_id'>,
		signal?: AbortSignal
	): Promise<Blob> {
		const revision = event.revision;
		const attachmentId = event.canonical_attachment_id;
		if (
			!event.source_id ||
			typeof revision !== 'number' ||
			!Number.isSafeInteger(revision) ||
			revision <= 0 ||
			!attachmentId
		) {
			throw new Error('Canonical event attachment identity is incomplete.');
		}
		const result = await this.fetchEventMediaObject(
			create(EventSearchMediaObjectSchema, {
				objectId: `attachment-${this.#nextStoredId++}`,
				sourceId: event.source_id,
				eventId: event.id,
				eventRevision: BigInt(revision),
				attachmentId,
				representation: StoredMediaObjectRepresentation.EVENT_ATTACHMENT
			}),
			signal
		);
		if (
			result.kind !== 'attachment' ||
			result.eventId !== event.id ||
			result.eventRevision !== revision ||
			result.attachmentId !== attachmentId
		) {
			throw new Error('Canonical event attachment identity changed during transfer.');
		}
		return new Blob([ownedArrayBuffer(result.payload)], { type: result.contentType });
	}

	private async fetchEventMediaObject(
		object: ProtoEventSearchMediaObject,
		signal?: AbortSignal
	): Promise<EventMediaResult> {
		if (signal?.aborted) throw timelineAbortError();
		const transferId = `event-media-${this.#nextStoredId++}`;
		const completed = new Promise<Map<string, EventMediaResult>>((resolve, reject) => {
			const timeout = setTimeout(() => {
				this.#eventMediaPending.delete(transferId);
				reject(new Error('Event media transfer timed out.'));
			}, controlTimeoutMs);
			this.#eventMediaPending.set(transferId, {
				objectIds: new Set([object.objectId]),
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
			const response = await this.request({ case: 'eventSearchCommand', value: command });
			if (
				response.case !== 'eventSearchMediaDelivery' ||
				response.value.transferId !== transferId ||
				response.value.channel !== DataChannelKind.RELIABLE_DATA ||
				response.value.objectCount !== 1
			) {
				throw new Error('Server returned an unexpected event media response.');
			}
			if (aborted) throw timelineAbortError();
			awaitingDelivery = true;
			if (signal?.aborted) abort();
			const result = (await completed).get(object.objectId);
			if (!result) throw new Error('Event media transfer omitted its requested object.');
			return result;
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
		return this.#system.getHealth(signal);
	}

	async listNotificationRules(): Promise<NotificationRuleRecord[]> {
		return this.#notifications.listRules();
	}

	async saveNotificationRuleDraft(
		rule: NotificationRuleDefinition,
		expectedDraftRevision: bigint
	): Promise<NotificationRuleRecord> {
		return this.#notifications.saveRuleDraft(rule, expectedDraftRevision);
	}

	async activateNotificationRule(
		ruleId: string,
		expectedActiveRevision: bigint,
		expectedDraftRevision: bigint
	): Promise<NotificationRuleRecord> {
		return this.#notifications.activateRule(ruleId, expectedActiveRevision, expectedDraftRevision);
	}

	async deleteNotificationRule(
		ruleId: string,
		expectedActiveRevision: bigint,
		expectedDraftRevision: bigint
	): Promise<void> {
		return this.#notifications.deleteRule(ruleId, expectedActiveRevision, expectedDraftRevision);
	}

	async testNotificationRule(ruleId: string): Promise<NotificationTestResult> {
		return this.#notifications.testRule(ruleId);
	}

	async getNotificationInbox(limit = 100): Promise<NotificationInbox> {
		return this.#notifications.getInbox(limit);
	}

	async getNotificationHistory(limit = 100): Promise<NotificationHistoryGroup[]> {
		return this.#notifications.getHistory(limit);
	}

	async markNotificationSeen(logicalId: string): Promise<void> {
		return this.#notifications.markSeen(logicalId);
	}

	async acknowledgeNotification(logicalId: string): Promise<void> {
		return this.#notifications.acknowledge(logicalId);
	}

	async clearNotification(logicalId: string): Promise<void> {
		return this.#notifications.clear(logicalId);
	}

	async clearNotifications(scope: NotificationClearScope): Promise<bigint> {
		return this.#notifications.clearScope(scope);
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
		event?: Pick<
			RecordingEvent,
			| 'id'
			| 'revision'
			| 'attachments'
			| 'canonical_attachment_id'
			| 'icon_key'
			| 'image_availability'
		>;
	}): Promise<MediaExportJob> {
		const jobId = `export-${crypto.randomUUID()}`;
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
					burnInTimestamp: options.burnInTimestamp ?? false,
					eventSeed: options.event ? protoExportEventSeed(options.event) : undefined
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
		return this.#system.getLoggingSettings();
	}

	async setLoggingFilter(filter: string): Promise<LoggingSettings> {
		return this.#system.setLoggingFilter(filter);
	}

	async restartServer(): Promise<void> {
		return this.#system.restartServer();
	}

	async revealAccessKey(): Promise<string> {
		return this.#system.revealAccessKey();
	}

	async rotateAccessKey(): Promise<string> {
		return this.#system.rotateAccessKey();
	}

	async getAccessSession(): Promise<AccessSession> {
		const command = create(ServerCommandSchema, {
			action: { case: 'getAccessSession', value: create(GetAccessSessionSchema) }
		});
		const result = await this.request({ case: 'serverCommand', value: command });
		if (result.case !== 'accessSessionResult' || !result.value.current) {
			throw new Error('Server returned an unexpected access session response.');
		}
		return accessSession(result.value.current);
	}

	async listAccessCredentials(): Promise<AccessCredential[]> {
		const command = create(ServerCommandSchema, {
			action: {
				case: 'listAccessCredentials',
				value: create(ListAccessCredentialsSchema)
			}
		});
		const result = await this.request({ case: 'serverCommand', value: command });
		if (result.case !== 'accessCredentialResult' || result.value.accessKey !== undefined) {
			throw new Error('Server returned an unexpected access credential response.');
		}
		return result.value.credentials.map(accessCredential);
	}

	async createAccessCredential(input: AccessCredentialInput): Promise<IssuedAccessCredential> {
		const command = create(ServerCommandSchema, {
			action: {
				case: 'createAccessCredential',
				value: create(CreateAccessCredentialSchema, {
					name: input.name,
					description: input.description || undefined,
					role: protoAccessRole(input.role),
					expiresAtMs: input.expiresAtMs === undefined ? undefined : BigInt(input.expiresAtMs)
				})
			}
		});
		return this.issuedAccessCredential(command);
	}

	async rotateAccessCredential(credentialId: string): Promise<IssuedAccessCredential> {
		const command = create(ServerCommandSchema, {
			action: {
				case: 'rotateAccessCredential',
				value: create(RotateAccessCredentialSchema, { credentialId })
			}
		});
		return this.issuedAccessCredential(command);
	}

	async setAccessCredentialEnabled(
		credentialId: string,
		enabled: boolean
	): Promise<AccessCredential> {
		const command = create(ServerCommandSchema, {
			action: {
				case: 'setAccessCredentialEnabled',
				value: create(SetAccessCredentialEnabledSchema, { credentialId, enabled })
			}
		});
		return this.accessCredentialMutation(command);
	}

	async revokeAccessCredential(credentialId: string): Promise<AccessCredential> {
		const command = create(ServerCommandSchema, {
			action: {
				case: 'revokeAccessCredential',
				value: create(RevokeAccessCredentialSchema, { credentialId })
			}
		});
		return this.accessCredentialMutation(command);
	}

	async listAccessSessions(): Promise<AccessSession[]> {
		const command = create(ServerCommandSchema, {
			action: { case: 'listAccessSessions', value: create(ListAccessSessionsSchema) }
		});
		const result = await this.request({ case: 'serverCommand', value: command });
		if (result.case !== 'accessSessionResult') {
			throw new Error('Server returned an unexpected access session list.');
		}
		return result.value.sessions.map(accessSession);
	}

	async revokeAccessSession(sessionId: string): Promise<void> {
		const command = create(ServerCommandSchema, {
			action: {
				case: 'revokeAccessSession',
				value: create(RevokeAccessSessionSchema, { sessionId })
			}
		});
		const result = await this.request({ case: 'serverCommand', value: command });
		if (result.case !== 'accessSessionResult') {
			throw new Error('Server did not acknowledge session revocation.');
		}
	}

	async listAccessAudit(limit = 100): Promise<AccessAuditEvent[]> {
		const command = create(ServerCommandSchema, {
			action: {
				case: 'listAccessAudit',
				value: create(ListAccessAuditSchema, { limit })
			}
		});
		const result = await this.request({ case: 'serverCommand', value: command });
		if (result.case !== 'accessAuditResult') {
			throw new Error('Server returned an unexpected access audit response.');
		}
		return result.value.events.map(accessAuditEvent);
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
		return this.#system.updateRuntimeConfiguration(update);
	}

	async getRuntimeConfiguration(): Promise<SanitizedConfig> {
		return this.#system.getRuntimeConfiguration();
	}

	async probeStorage(path: string): Promise<StorageWriteProbe> {
		return this.#system.probeStorage(path);
	}

	async close(): Promise<void> {
		const sessionId = this.release();
		if (sessionId !== null) await deleteSession(sessionId, this.#accessKey);
	}

	closeOnPageHide(): void {
		const sessionId = this.release();
		if (sessionId === null) return;
		void deleteSession(sessionId, this.#accessKey, { keepalive: true }).catch(() => undefined);
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

	private async issuedAccessCredential(
		command: ReturnType<typeof create<typeof ServerCommandSchema>>
	): Promise<IssuedAccessCredential> {
		const result = await this.request({ case: 'serverCommand', value: command });
		if (
			result.case !== 'accessCredentialResult' ||
			result.value.credentials.length !== 1 ||
			!result.value.accessKey
		) {
			throw new Error('Server did not return the issued access credential.');
		}
		return {
			credential: accessCredential(result.value.credentials[0]!),
			accessKey: result.value.accessKey
		};
	}

	private async accessCredentialMutation(
		command: ReturnType<typeof create<typeof ServerCommandSchema>>
	): Promise<AccessCredential> {
		const result = await this.request({ case: 'serverCommand', value: command });
		if (
			result.case !== 'accessCredentialResult' ||
			result.value.credentials.length !== 1 ||
			result.value.accessKey !== undefined
		) {
			throw new Error('Server returned an unexpected access credential mutation.');
		}
		return accessCredential(result.value.credentials[0]!);
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
			if (peer !== this.#peer) return;
			const accessKey = this.#accessKey;
			const sessionId = this.releaseTransport(false);
			this.handleUnexpectedAccessDisconnect();
			if (sessionId !== null) {
				void deleteSession(sessionId, accessKey).catch(() => undefined);
			}
		};
		reliable.onclose = () => this.failData('WebRTC reliable data channel closed.');
		peer.onconnectionstatechange = () => {
			if (
				peer === this.#peer &&
				['failed', 'disconnected', 'closed'].includes(peer.connectionState)
			) {
				const accessKey = this.#accessKey;
				const sessionId = this.releaseTransport(false);
				this.handleUnexpectedAccessDisconnect();
				if (sessionId !== null) {
					void deleteSession(sessionId, accessKey).catch(() => undefined);
				}
			}
		};

		try {
			const offer = await peer.createOffer();
			await peer.setLocalDescription(offer);
			if (!peer.localDescription) throw new Error('WebRTC offer is unavailable.');
			const session = await createSession(peer.localDescription, this.#accessKey);
			if (peer !== this.#peer) {
				await deleteSession(session.session_id, this.#accessKey);
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
			if (sessionId !== null) {
				await deleteSession(sessionId, this.#accessKey).catch(() => undefined);
			}
			this.publishConnectionError(error);
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
				if (capabilities.accessSession) {
					this.publishAccessState({
						status: 'authenticated',
						session: accessSession(capabilities.accessSession),
						message: null
					});
				}
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
		const isKeyframe = chunk.representation === StoredMediaObjectRepresentation.ENCODED_KEYFRAME;
		const isAttachment = chunk.representation === StoredMediaObjectRepresentation.EVENT_ATTACHMENT;
		if (
			(!isKeyframe && !isAttachment) ||
			chunk.chunkCount === 0 ||
			chunk.chunkIndex >= chunk.chunkCount ||
			byteLength <= 0 ||
			byteLength > maxEventKeyframeBytes ||
			(isAttachment &&
				(!chunk.eventId ||
					chunk.eventRevision === 0n ||
					!chunk.attachmentId ||
					!['image/jpeg', 'image/png', 'image/webp'].includes(chunk.contentType)))
		) {
			this.failEventMediaPending(chunk.transferId, 'Event media chunk was invalid or oversized.');
			return;
		}
		const accumulator = pending.objects.get(chunk.objectId) ?? {
			representation: chunk.representation,
			chunkCount: chunk.chunkCount,
			chunks: Array.from<Uint8Array | undefined>({ length: chunk.chunkCount }),
			byteLength,
			receivedBytes: 0,
			contentType: chunk.contentType,
			codec: chunk.codec,
			width: chunk.width,
			height: chunk.height,
			decoderConfig: chunk.decoderConfig,
			nalLengthSize: chunk.nalLengthSize,
			eventId: chunk.eventId,
			eventRevision: chunk.eventRevision,
			attachmentId: chunk.attachmentId
		};
		if (
			accumulator.representation !== chunk.representation ||
			accumulator.chunkCount !== chunk.chunkCount ||
			accumulator.byteLength !== byteLength ||
			accumulator.contentType !== chunk.contentType ||
			accumulator.codec !== chunk.codec ||
			accumulator.width !== chunk.width ||
			accumulator.height !== chunk.height ||
			accumulator.nalLengthSize !== chunk.nalLengthSize ||
			accumulator.eventId !== chunk.eventId ||
			accumulator.eventRevision !== chunk.eventRevision ||
			accumulator.attachmentId !== chunk.attachmentId ||
			!bytesEqual(accumulator.decoderConfig, chunk.decoderConfig) ||
			accumulator.chunks[chunk.chunkIndex] !== undefined ||
			accumulator.receivedBytes + chunk.payload.byteLength > accumulator.byteLength
		) {
			this.failEventMediaPending(chunk.transferId, 'Event media chunks were inconsistent.');
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
			pending.reject(new Error('Event media delivery was incomplete.'));
			return;
		}
		const objects = new Map<string, EventMediaResult>();
		for (const objectId of pending.objectIds) {
			const object = pending.objects.get(objectId);
			if (
				!object ||
				object.receivedBytes !== object.byteLength ||
				!object.chunks.every((chunk) => chunk !== undefined)
			) {
				pending.reject(new Error('Event media delivery was incomplete.'));
				return;
			}
			const payload = concatenateChunks(object.chunks);
			if (object.representation === StoredMediaObjectRepresentation.EVENT_ATTACHMENT) {
				objects.set(objectId, {
					kind: 'attachment',
					eventId: object.eventId,
					eventRevision: numeric(object.eventRevision),
					attachmentId: object.attachmentId,
					contentType: object.contentType,
					payload
				});
			} else {
				objects.set(objectId, {
					kind: 'keyframe',
					media: {
						contentType: object.contentType,
						codec: object.codec,
						width: object.width,
						height: object.height,
						decoderConfig: object.decoderConfig,
						nalLengthSize: object.nalLengthSize,
						payload
					}
				});
			}
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
				error.message || 'Event media transfer failed.'
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

	private publishAccessState(
		state: Omit<AccessConnectionState, 'generation'> & { generation?: number }
	): void {
		const authenticatedSessionChanged =
			state.status === 'authenticated' && state.session?.id !== this.#accessState.session?.id;
		this.#accessState = {
			...state,
			generation:
				state.generation ?? this.#accessState.generation + (authenticatedSessionChanged ? 1 : 0)
		};
		for (const listener of this.#accessStateListeners) listener(this.#accessState);
	}

	private publishConnectionError(error: unknown): void {
		if (error instanceof ApiRequestError && error.status === 401) {
			const hadCredential = this.#accessKey !== null;
			this.#accessKey = null;
			this.publishAccessState({
				status: 'sign-in-required',
				session: null,
				message: hadCredential ? 'The access key is invalid, expired, or revoked.' : null
			});
			return;
		}
		const message =
			error instanceof ApiRequestError && error.status === 426
				? 'Remote access requires HTTPS or a configured trusted proxy.'
				: error instanceof ApiRequestError && error.status === 429
					? 'Too many failed sign-in attempts. Try again shortly.'
					: 'KeepPeek could not establish a secure session.';
		this.publishAccessState({ status: 'error', session: null, message });
	}

	private async authenticatedHttp<T>(
		request: () => Promise<T>,
		localStatuses: readonly number[] = []
	): Promise<T> {
		try {
			return await request();
		} catch (error) {
			if (
				!(error instanceof DOMException && error.name === 'AbortError') &&
				!(error instanceof ApiRequestError && localStatuses.includes(error.status))
			) {
				this.publishConnectionError(error);
			}
			throw error;
		}
	}

	private handleUnexpectedAccessDisconnect(): void {
		const remote = this.#accessState.session?.local === false || this.#accessKey !== null;
		if (remote) this.#accessKey = null;
		this.publishAccessState({
			status: remote ? 'sign-in-required' : 'error',
			session: null,
			message: remote
				? 'The remote session expired, was revoked, or disconnected.'
				: 'The local session disconnected.'
		});
	}

	private release(): string | null {
		return this.releaseTransport(true);
	}

	private releaseTransport(revokeObjectUrls: boolean): string | null {
		const sessionId = this.#sessionId;
		const controlChannel = this.#controlChannel;
		const reliableChannel = this.#reliableChannel;
		const unreliableChannel = this.#unreliableChannel;
		const peer = this.#peer;
		this.#sessionId = null;
		this.#controlChannel = null;
		this.#reliableChannel = null;
		this.#unreliableChannel = null;
		this.#peer = null;
		controlChannel?.close();
		reliableChannel?.close();
		unreliableChannel?.close();
		peer?.close();
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

function protoAccessRole(role: AccessRole): ProtoAccessRole {
	return role === 'administrator' ? ProtoAccessRole.ADMINISTRATOR : ProtoAccessRole.USER;
}

function accessRole(role: ProtoAccessRole): AccessRole {
	if (role === ProtoAccessRole.ADMINISTRATOR) return 'administrator';
	if (role === ProtoAccessRole.USER) return 'user';
	throw new Error('Server returned an invalid access role.');
}

function optionalAccessRole(role: ProtoAccessRole): AccessRole | null {
	return role === ProtoAccessRole.UNSPECIFIED ? null : accessRole(role);
}

function accessSession(session: ProtoAccessSession): AccessSession {
	return {
		id: session.sessionId,
		principalId: session.principalId,
		displayName: session.displayName,
		role: accessRole(session.role),
		local: session.local,
		clientClassification: session.clientClassification,
		createdAtMs: Number(session.createdAtMs),
		lastActivityAtMs: Number(session.lastActivityAtMs),
		absoluteExpiresAtMs: Number(session.absoluteExpiresAtMs),
		credentialExpiresAtMs:
			session.credentialExpiresAtMs === undefined ? null : Number(session.credentialExpiresAtMs)
	};
}

function accessCredential(credential: ProtoAccessCredential): AccessCredential {
	return {
		id: credential.credentialId,
		name: credential.name,
		description: credential.description ?? null,
		role: accessRole(credential.role),
		createdAtMs: Number(credential.createdAtMs),
		rotatedAtMs: credential.rotatedAtMs === undefined ? null : Number(credential.rotatedAtMs),
		lastUsedAtMs: credential.lastUsedAtMs === undefined ? null : Number(credential.lastUsedAtMs),
		expiresAtMs: credential.expiresAtMs === undefined ? null : Number(credential.expiresAtMs),
		disabled: credential.disabled,
		revokedAtMs: credential.revokedAtMs === undefined ? null : Number(credential.revokedAtMs),
		revision: credential.revision,
		initialAccessKeyPending: credential.initialAccessKeyPending
	};
}

function accessAuditEvent(event: ProtoAccessAuditEvent): AccessAuditEvent {
	return {
		id: event.eventId,
		timestampMs: Number(event.timestampMs),
		principalId: event.principalId ?? null,
		role: optionalAccessRole(event.role),
		action: event.action,
		targetId: event.targetId ?? null,
		result: event.result,
		clientClassification: event.clientClassification
	};
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
	const eventAttachments = event.attachments.map(recordingEventAttachment);
	const canonicalAttachment = canonicalEventAttachment(
		eventAttachments,
		event.canonicalAttachmentId
	);
	const descriptor = canonicalAttachment
		? event.attachments.find((attachment) => attachment.attachmentId === canonicalAttachment.id)
		: undefined;
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
		bbox_attachment_id: event.boundingBoxAttachmentId ?? null,
		zone: event.zone ?? null,
		text: event.text ?? null,
		operational: operationalEventEvidence(event),
		thumbnail_url: thumbnailUrl,
		thumbnail_blob: thumbnailBlob,
		attachments: eventAttachments,
		canonical_attachment_id: canonicalAttachment?.id ?? null,
		icon_key: eventIconKey(event.iconKey, event.eventType),
		rejected_icon_key: event.rejectedIconKey ?? null,
		image_availability: eventImageAvailability(
			event.imageAvailability,
			canonicalAttachment !== null
		)
	};
}

function recordingEventAttachment(
	attachment: ProtoEventAttachmentDescriptor
): RecordingEventAttachment {
	return {
		id: attachment.attachmentId,
		type: attachment.attachmentType,
		content_type: attachment.contentType,
		byte_length: attachment.byteLen === undefined ? null : numeric(attachment.byteLen),
		ordinal: attachment.ordinal,
		timestamp_ms: attachment.timestamp ? timestampDate(attachment.timestamp).getTime() : null,
		text: attachment.text ?? null
	};
}

function eventImageAvailability(
	availability: ProtoEventImageAvailability,
	hasCanonicalImage: boolean
): EventImageAvailabilityState {
	if (availability === ProtoEventImageAvailability.AVAILABLE) return 'available';
	if (availability === ProtoEventImageAvailability.UNAVAILABLE) return 'unavailable';
	if (availability === ProtoEventImageAvailability.NONE) return 'none';
	return hasCanonicalImage ? 'available' : 'none';
}

function operationalEventEvidence(event: ProtoEvent): RecordingEvent['operational'] {
	if (
		event.eventType !== 'camera_offline' &&
		event.eventType !== 'stream_stale' &&
		event.eventType !== 'decode_unavailable' &&
		event.eventType !== 'recording_interrupted'
	) {
		return null;
	}
	const severity = eventPayloadString(event, 'severity');
	return {
		kind: event.eventType,
		severity: severity === 'critical' ? 'critical' : 'warning',
		cause: eventPayloadString(event, 'cause') ?? 'evidence_unavailable',
		explanation: event.text ?? '',
		affected_streams: eventPayloadStrings(event, 'affected_streams'),
		recording_interrupted: eventPayloadBoolean(event, 'recording_interrupted') ?? false,
		evidence_source: eventPayloadString(event, 'evidence_source') ?? 'canonical_health',
		stream_id: eventPayloadString(event, 'stream_id'),
		duration_ms: eventPayloadNumber(event, 'duration_ms'),
		recovered: eventPayloadBoolean(event, 'recovered') ?? event.endTime !== undefined
	};
}

function eventPayloadString(event: ProtoEvent, key: string): string | null {
	const value = event.payload?.[key];
	return typeof value === 'string' ? value : null;
}

function eventPayloadBoolean(event: ProtoEvent, key: string): boolean | null {
	const value = event.payload?.[key];
	return typeof value === 'boolean' ? value : null;
}

function eventPayloadNumber(event: ProtoEvent, key: string): number | null {
	const value = event.payload?.[key];
	return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

function eventPayloadStrings(event: ProtoEvent, key: string): string[] {
	const value = event.payload?.[key];
	return Array.isArray(value) && value.every((item) => typeof item === 'string') ? value : [];
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
	const canonicalAttachment = hit.canonicalAttachment
		? recordingEventAttachment(hit.canonicalAttachment)
		: null;
	const attachments = hit.attachments.map(recordingEventAttachment);
	return {
		eventId: hit.eventId,
		revision: numeric(hit.revision),
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
		hasImageAttachment: canonicalAttachment !== null || hit.hasImageAttachment,
		canonicalAttachment,
		attachments,
		imageAvailability: eventImageAvailability(
			hit.imageAvailability,
			canonicalAttachment !== null || hit.hasImageAttachment
		),
		iconKey: eventIconKey(hit.iconKey, hit.eventType),
		rejectedIconKey: hit.rejectedIconKey ?? null,
		bboxAttachmentId: hit.boundingBoxAttachmentId ?? null,
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
		burnInTimestamp: job.burnInTimestamp,
		eventSeed: job.eventSeed
			? {
					eventId: job.eventSeed.eventId,
					revision: numeric(job.eventSeed.revision),
					canonicalAttachment: job.eventSeed.canonicalAttachment
						? recordingEventAttachment(job.eventSeed.canonicalAttachment)
						: null,
					iconKey: job.eventSeed.iconKey ? eventIconKey(job.eventSeed.iconKey, '') : undefined,
					imageAvailability: eventImageAvailability(
						job.eventSeed.imageAvailability,
						job.eventSeed.canonicalAttachment !== undefined
					)
				}
			: null
	};
}

function protoExportEventSeed(
	event: Pick<
		RecordingEvent,
		| 'id'
		| 'revision'
		| 'attachments'
		| 'canonical_attachment_id'
		| 'icon_key'
		| 'image_availability'
	>
) {
	if (!Number.isSafeInteger(event.revision) || (event.revision ?? 0) <= 0) {
		throw new Error('Export event revision is incomplete.');
	}
	const canonical = canonicalEventAttachment(
		event.attachments ?? [],
		event.canonical_attachment_id
	);
	return create(EventExportSeedSchema, {
		eventId: event.id,
		revision: BigInt(event.revision!),
		canonicalAttachment: canonical
			? create(EventAttachmentDescriptorSchema, {
					attachmentId: canonical.id,
					attachmentType: canonical.type,
					contentType: canonical.content_type,
					byteLen: canonical.byte_length === null ? undefined : BigInt(canonical.byte_length),
					ordinal: canonical.ordinal,
					timestamp:
						canonical.timestamp_ms === null
							? undefined
							: timestampFromDate(new Date(canonical.timestamp_ms)),
					text: canonical.text ?? undefined
				})
			: undefined,
		iconKey: event.icon_key,
		imageAvailability:
			event.image_availability === 'available'
				? ProtoEventImageAvailability.AVAILABLE
				: event.image_availability === 'unavailable'
					? ProtoEventImageAvailability.UNAVAILABLE
					: ProtoEventImageAvailability.NONE
	});
}

function motionDetection(result: MotionDetectionResult): MotionDetection {
	return {
		supported: result.supported,
		controllable: result.controllable,
		enabled: result.enabled ?? null,
		error: result.error ?? null
	};
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
