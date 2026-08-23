import { createHash } from 'node:crypto';
import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import { durationFromMs, timestampFromDate } from '@bufbuild/protobuf/wkt';
import type { Page } from '@playwright/test';
import {
	CameraDiscoveryResultSchema,
	CameraCatalogCameraSchema,
	CameraCatalogInfoSchema,
	CameraCatalogSearchResultSchema,
	CameraCatalogStreamHintsSchema,
	CameraCatalogStreamSchema,
	CameraStreamProbeResultSchema,
	CameraBackend as ProtoCameraBackend,
	CameraConfigurationResultSchema,
	CameraDeviceCapabilitiesSchema,
	CameraInfoSchema,
	CameraManufacturerResultSchema,
	CameraSettingsSchema,
	CameraHealthSnapshotSchema,
	CatalogHealthSnapshotSchema,
	CameraTransport as ProtoCameraTransport,
	ControlEnvelopeSchema,
	CodecDescriptorSchema,
	DataChannelKind,
	DeliveryTransport,
	DiscoveredCameraSchema,
	DiskHealthSnapshotSchema,
	EventAttachmentDescriptorSchema,
	EventAttachmentChunkSchema,
	EventBoundingBoxSchema,
	EventMessageSchema,
	EventOrigin,
	EventSchema,
	ErrorCode,
	ErrorSchema,
	ExportDownloadResultSchema,
	ExportFileChunkSchema,
	ExportJobListSchema,
	ExportJobSchema,
	ExportJobStatus,
	ExportMessageSchema,
	ExportMissingRangeSchema,
	HealthAudioProfileSummarySchema,
	HealthIssueSnapshotSchema,
	HealthProfileSummarySchema,
	HealthTotalsSnapshotSchema,
	LogBufferStatsSchema,
	LoadHealthSnapshotSchema,
	LoggingSettingsResultSchema,
	MediaKind,
	MotionDetectionResultSchema,
	MessageSchema,
	MediaDataFormatSchema,
	MemoryHealthSnapshotSchema,
	NetworkHealthSnapshotSchema,
	OkSchema,
	NotificationSchema,
	PtzCapabilitySchema,
	PtzPresetSchema,
	PtzResultSchema,
	ProcessHealthSnapshotSchema,
	RestartResultSchema,
	RecordingCapacityEstimateSchema,
	RecordingDemandHealthSnapshotSchema,
	RecordingDemandStreamHealthSnapshotSchema,
	ResponseSchema,
	RtpDeliverySchema,
	RuntimeConfigurationResultSchema,
	RuntimeStorageConfigurationSchema,
	SanitizedRuntimeConfigurationSchema,
	ServerCapabilitiesSchema,
	ServerHealthSnapshotSchema,
	SourceSessionSchema,
	MediaStreamCapabilitySchema,
	MediaVariantCapabilitySchema,
	StoredMediaDeliverySchema,
	StoredMediaQueryDeliverySchema,
	StoredMediaQueryEndSchema,
	StoredMediaQueryMessageSchema,
	StoredMediaQueryPageSchema,
	StoredMediaRangeSchema,
	StoredMediaStateSchema,
	StoredMediaStatus,
	StoredMediaSourceCapabilitySchema,
	StoredMediaStreamCapabilitySchema,
	SubscriptionResultSchema,
	StorageHealthSnapshotSchema,
	StreamHealthSnapshotSchema,
	SystemHealthSnapshotSchema,
	TemperatureHealthSnapshotSchema,
	CpuHealthSnapshotSchema,
	VideoDataFormatSchema,
	WebRtcHealthSnapshotSchema,
	WebRtcSessionQueueHealthSnapshotSchema,
	WebRtcSourceHealthSnapshotSchema,
	type Ok,
	type ExportJob,
	type StoredMediaState
} from '../../src/lib/proto/webrtc_pb';
import type {
	CameraSettings,
	CameraSettingsUpdate,
	CameraSettingsUpdateResponse,
	CameraCatalogCamera,
	CameraCatalogInfo,
	CameraListItem,
	DiscoveredCameraSettings,
	MotionDetection,
	RecordingEvent,
	SanitizedConfig,
	ServerHealthResponse,
	SettingsConfigUpdate,
	SettingsConfigUpdateResponse
} from '../../src/lib/types';

type DeepPartial<T> = T extends readonly (infer Item)[]
	? DeepPartial<Item>[]
	: T extends object
		? { [Key in keyof T]?: DeepPartial<T[Key]> }
		: T;

export type HealthFixture = DeepPartial<ServerHealthResponse>;

const defaultCameraCatalog: CameraCatalogInfo = {
	version: '2.1.0',
	tag: 'v2.1.0',
	generated_at: '2026-08-22T06:13:00Z',
	camera_count: 3433,
	website_url: 'https://www.cctv-database.com/'
};

export type StoredRangeFixture = {
	sourceId: string;
	streamId: 'main' | 'sub';
	startMs: number;
	endMs: number;
};

export type StoredEventFixture = {
	sourceId: string;
	event: RecordingEvent;
	thumbnail?: Uint8Array;
};

export type MotionControlRequest = {
	sourceId: string;
	enabled: boolean;
};

export type ManufacturerControlRequest = {
	sourceId: string;
	manufacturer: string | null;
};

export type PtzControlRequest = {
	sourceId: string;
	action: 'continuous' | 'stop' | 'listPresets' | 'gotoPreset';
	pan?: number;
	tilt?: number;
	zoom?: number;
	presetId?: number;
};

export type ExportJobFixture = {
	jobId?: string;
	sourceId?: string;
	streamId?: 'main' | 'sub';
	requestedStartMs?: number;
	requestedEndMs?: number;
	alignedStartMs?: number;
	status: 'running' | 'partial' | 'ready' | 'failed' | 'cancelled' | 'expired';
	progress?: number;
	bytesWritten?: number;
	estimatedBytes?: number;
	fileName?: string;
	sha256?: string;
	expiresAtMs?: number;
	missingRanges?: Array<{ startMs: number; endMs: number }>;
	error?: string;
	retryable?: boolean;
	burnInTimestamp?: boolean;
};

export type ExportControlRequest = {
	action: 'create' | 'list' | 'get' | 'cancel' | 'retry' | 'download';
	jobId?: string;
	allowPartial?: boolean;
	burnInTimestamp?: boolean;
};

export type ControlRequests = {
	motion: MotionControlRequest[];
	ptz: PtzControlRequest[];
	manufacturer: ManufacturerControlRequest[];
	loggingFilters: string[];
	restarts: number;
	discoverySubnets: number[][];
	catalogSearches: Array<{ query: string; limit: number | undefined; ip: string | undefined }>;
	cameraUpdates: Array<{ ip: string; update: CameraSettingsUpdate }>;
	removedCameraIps: string[];
	runtimeUpdates: SettingsConfigUpdate[];
	storedOpens: Array<{ storedMediaId: string; sourceId: string; streamId: string }>;
	storedSeeks: Array<{ storedMediaId: string; timestampMs: number }>;
	storedRefills: Array<{ storedMediaId: string; playbackTimeMs: number }>;
	exportJobs: ExportControlRequest[];
	streamProbes: Array<{ ip: string; onvifPort: number | null }>;
};

export type MockControlPeerOptions = {
	reportedManufacturer?: string;
	discoveredCameras?: readonly DiscoveredCameraSettings[];
	cameraCatalog?: CameraCatalogInfo;
	cameraCatalogSearchResults?: readonly CameraCatalogCamera[];
	streamProbeResult?: {
		main_rtsp_url: string | null;
		sub_rtsp_url: string | null;
		onvif_port?: number | null;
	};
	streamProbeGate?: Promise<void>;
	discoveryGate?: Promise<void>;
	cameraUpdateResult?: CameraSettingsUpdateResponse;
	cameraUpdateError?: string;
	cameraSettings?: readonly CameraSettings[];
	runtimeConfiguration?: SanitizedConfig;
	health?: HealthFixture;
	motionDetection?: MotionDetection;
	ptzPresets?: readonly { id: number; name: string }[];
	runtimeUpdateResult?: SettingsConfigUpdateResponse;
	runtimeUpdateGate?: Promise<void>;
	cameras?: readonly CameraListItem[];
	healthGate?: Promise<void>;
	storedRanges?: readonly StoredRangeFixture[];
	storedEvents?: readonly StoredEventFixture[];
	storedOpenGates?: readonly Promise<void>[];
	capabilityIds?: readonly string[];
	exportJobs?: readonly ExportJobFixture[];
	exportCreateResults?: readonly ExportJobFixture[];
	exportGetResults?: readonly ExportJobFixture[];
	exportRetryResult?: ExportJobFixture;
	exportFile?: Uint8Array;
};

function encodedOk(requestId: bigint, result?: Ok['result']): number[] {
	const response = create(ControlEnvelopeSchema, {
		message: {
			case: 'response',
			value: create(ResponseSchema, {
				requestId,
				result: { case: 'ok', value: create(OkSchema, { result }) }
			})
		}
	});
	return Array.from(toBinary(ControlEnvelopeSchema, response));
}

function encodedError(requestId: bigint, message: string): number[] {
	const response = create(ControlEnvelopeSchema, {
		message: {
			case: 'response',
			value: create(ResponseSchema, {
				requestId,
				result: {
					case: 'error',
					value: create(ErrorSchema, { code: ErrorCode.INVALID_REQUEST, message })
				}
			})
		}
	});
	return Array.from(toBinary(ControlEnvelopeSchema, response));
}

function encodedData(message: ReturnType<typeof create<typeof MessageSchema>>): number[] {
	return Array.from(toBinary(MessageSchema, message));
}

export async function mockControlPeer(
	page: Page,
	options: MockControlPeerOptions = {}
): Promise<ControlRequests> {
	const reportedManufacturer = options.reportedManufacturer ?? 'ONVIF';
	const requests: ControlRequests = {
		motion: [],
		ptz: [],
		manufacturer: [],
		loggingFilters: [],
		restarts: 0,
		discoverySubnets: [],
		catalogSearches: [],
		streamProbes: [],
		cameraUpdates: [],
		removedCameraIps: [],
		runtimeUpdates: [],
		storedOpens: [],
		storedSeeks: [],
		storedRefills: [],
		exportJobs: []
	};
	let activeFilter = 'info,keeppeek=debug';
	const pendingDataMessages: number[][] = [];
	const storedCursors = new Map<string, StoredMediaState>();
	const storedOpenGates = [...(options.storedOpenGates ?? [])];
	const exportFile = options.exportFile ?? Uint8Array.from([0, 0, 0, 8, 102, 116, 121, 112]);
	const exportFileHash = createHash('sha256').update(exportFile).digest('hex');
	const exportJobs = new Map<string, ExportJob>();
	for (const fixture of options.exportJobs ?? []) {
		const exportJob = fixtureExportJob(fixture, exportFileHash);
		exportJobs.set(exportJob.jobId, exportJob);
	}
	const exportCreateResults = [...(options.exportCreateResults ?? [])];
	const exportGetResults = [...(options.exportGetResults ?? [])];
	await page.exposeFunction('takeKeepPeekData', () => pendingDataMessages.splice(0));
	await page.exposeFunction('getKeepPeekCapabilities', () =>
		encodedCapabilities(
			options.cameras ?? [],
			options.storedRanges ?? [],
			options.capabilityIds ?? []
		)
	);
	await page.exposeFunction('handleKeepPeekControl', async (payload: number[]) => {
		const envelope = fromBinary(ControlEnvelopeSchema, Uint8Array.from(payload));
		if (envelope.message.case !== 'request') throw new Error('expected control request');
		const request = envelope.message.value;
		if (request.command.case === 'subscribeMedia') {
			return encodedOk(request.requestId, {
				case: 'subscriptionResult',
				value: create(SubscriptionResultSchema, {
					subscriptionId: request.command.value.subscriptionId,
					delivery: { case: 'rtp', value: create(RtpDeliverySchema, { mid: '0' }) },
					selectedVariantId: request.command.value.variantId
				})
			});
		}
		if (request.command.case === 'unsubscribe') {
			return encodedOk(request.requestId, undefined);
		}
		if (request.command.case === 'storedMediaCommand') {
			const action = request.command.value.action;
			if (action.case === 'queryTimeline') {
				const query = action.value;
				const startMs = query.startTime ? timestampFromProto(query.startTime) : 0;
				const endMs = query.endTime ? timestampFromProto(query.endTime) : Number.MAX_SAFE_INTEGER;
				const sourceIds = new Set(query.sourceIds);
				const ranges = (options.storedRanges ?? []).filter(
					(range) =>
						(sourceIds.size === 0 || sourceIds.has(range.sourceId)) &&
						range.startMs < endMs &&
						range.endMs > startMs
				);
				const events = query.events
					? (options.storedEvents ?? []).filter(
							(fixture) =>
								(sourceIds.size === 0 || sourceIds.has(fixture.sourceId)) &&
								fixture.event.start_time_ms < endMs &&
								(fixture.event.end_time_ms ?? fixture.event.start_time_ms) >= startMs &&
								(query.events?.eventTypes.length === 0 ||
									query.events?.eventTypes.includes(fixture.event.kind))
						)
					: [];
				let pageCount = 0n;
				if (ranges.length > 0 || events.length > 0) {
					pageCount = 1n;
					pendingDataMessages.push(
						encodedData(
							create(MessageSchema, {
								message: {
									case: 'storedMediaQuery',
									value: create(StoredMediaQueryMessageSchema, {
										message: {
											case: 'page',
											value: create(StoredMediaQueryPageSchema, {
												queryId: query.queryId,
												sequence: 1n,
												availability: ranges.map((range) =>
													create(StoredMediaRangeSchema, {
														sourceId: range.sourceId,
														streamId: range.streamId,
														startTime: timestampFromDate(new Date(range.startMs)),
														endTime: timestampFromDate(new Date(range.endMs))
													})
												),
												events: events.map((fixture) => protoEvent(fixture))
											})
										}
									})
								}
							})
						)
					);
				}
				let attachmentCount = 0n;
				if (query.events?.includeAttachments) {
					for (const fixture of events) {
						if (!fixture.thumbnail) continue;
						attachmentCount += 1n;
						pendingDataMessages.push(
							encodedData(
								create(MessageSchema, {
									message: {
										case: 'event',
										value: create(EventMessageSchema, {
											message: {
												case: 'attachment',
												value: create(EventAttachmentChunkSchema, {
													context: { case: 'queryId', value: query.queryId },
													eventId: fixture.event.id,
													revision: 1n,
													attachmentId: 'thumbnail',
													attachmentType: 'thumbnail',
													contentType: 'image/jpeg',
													ordinal: 0,
													timestamp: timestampFromDate(new Date(fixture.event.start_time_ms)),
													sequence: attachmentCount,
													chunkIndex: 0,
													chunkCount: 1,
													payload: fixture.thumbnail
												})
											}
										})
									}
								})
							)
						);
					}
				}
				pendingDataMessages.push(
					encodedData(
						create(MessageSchema, {
							message: {
								case: 'storedMediaQuery',
								value: create(StoredMediaQueryMessageSchema, {
									message: {
										case: 'end',
										value: create(StoredMediaQueryEndSchema, {
											queryId: query.queryId,
											pageCount,
											attachmentCount
										})
									}
								})
							}
						})
					)
				);
				return encodedOk(request.requestId, {
					case: 'storedMediaQueryDelivery',
					value: create(StoredMediaQueryDeliverySchema, {
						queryId: query.queryId,
						channel: DataChannelKind.RELIABLE_DATA
					})
				});
			}
			if (action.case === 'open') {
				await storedOpenGates.shift();
				const open = action.value;
				requests.storedOpens.push({
					storedMediaId: open.storedMediaId,
					sourceId: open.sourceId,
					streamId: open.streamId
				});
				const state = create(StoredMediaStateSchema, {
					storedMediaId: open.storedMediaId,
					status: StoredMediaStatus.ACTIVE,
					generation: 1n,
					requestedTime: open.timestamp,
					fragmentTime: open.timestamp,
					endTime: open.endTime,
					mode: open.mode,
					playing: open.playing,
					playbackRate: open.playbackRate,
					delivery: create(StoredMediaDeliverySchema, {
						mediaChannel: DataChannelKind.RELIABLE_DATA,
						contentType: 'video/mp4; codecs="avc1.42E01E"',
						maxBufferDuration: durationFromMs(120_000)
					})
				});
				storedCursors.set(open.storedMediaId, state);
				return encodedOk(request.requestId, { case: 'storedMediaState', value: state });
			}
			if (action.case === 'setPlayback') {
				const current = storedCursors.get(action.value.storedMediaId);
				if (!current) return encodedError(request.requestId, 'stored media cursor not found');
				const state = create(StoredMediaStateSchema, {
					...current,
					playing: action.value.playing ?? current.playing,
					playbackRate: action.value.playbackRate ?? current.playbackRate,
					mode: action.value.mode ?? current.mode
				});
				storedCursors.set(action.value.storedMediaId, state);
				return encodedOk(request.requestId, { case: 'storedMediaState', value: state });
			}
			if (action.case === 'seek') {
				await storedOpenGates.shift();
				const current = storedCursors.get(action.value.storedMediaId);
				if (!current) return encodedError(request.requestId, 'stored media cursor not found');
				requests.storedSeeks.push({
					storedMediaId: action.value.storedMediaId,
					timestampMs: action.value.timestamp ? timestampFromProto(action.value.timestamp) : 0
				});
				const state = create(StoredMediaStateSchema, {
					...current,
					generation: current.generation + 1n,
					requestedTime: action.value.timestamp,
					fragmentTime: action.value.timestamp
				});
				storedCursors.set(action.value.storedMediaId, state);
				return encodedOk(request.requestId, { case: 'storedMediaState', value: state });
			}
			if (action.case === 'refill') {
				const current = storedCursors.get(action.value.storedMediaId);
				if (!current) return encodedError(request.requestId, 'stored media cursor not found');
				requests.storedRefills.push({
					storedMediaId: action.value.storedMediaId,
					playbackTimeMs: action.value.playbackTime
						? timestampFromProto(action.value.playbackTime)
						: 0
				});
				const state = create(StoredMediaStateSchema, {
					...current,
					status: StoredMediaStatus.ENDED
				});
				storedCursors.set(action.value.storedMediaId, state);
				return encodedOk(request.requestId, { case: 'storedMediaState', value: state });
			}
			if (action.case === 'close') {
				storedCursors.delete(action.value.storedMediaId);
				return encodedOk(request.requestId, undefined);
			}
			if (action.case === 'cancelTimelineQuery') {
				return encodedOk(request.requestId, undefined);
			}
			throw new Error(`unexpected stored media action ${action.case}`);
		}
		if (request.command.case === 'exportCommand') {
			const action = request.command.value.action;
			if (action.case === undefined) throw new Error('export action is empty');
			requests.exportJobs.push({
				action: action.case,
				...('jobId' in action.value ? { jobId: action.value.jobId } : {}),
				...(action.case === 'create'
					? {
							allowPartial: action.value.allowPartial,
							burnInTimestamp: action.value.burnInTimestamp
						}
					: {})
			});
			if (action.case === 'list') {
				return encodedOk(request.requestId, {
					case: 'exportJobs',
					value: create(ExportJobListSchema, { jobs: [...exportJobs.values()] })
				});
			}
			if (action.case === 'create') {
				const command = action.value;
				const base = create(ExportJobSchema, {
					jobId: command.jobId,
					sourceId: command.sourceId,
					streamId: command.streamId,
					requestedStartTime: command.startTime,
					requestedEndTime: command.endTime,
					status: command.burnInTimestamp ? ExportJobStatus.FAILED : ExportJobStatus.RUNNING,
					error: command.burnInTimestamp
						? 'Timestamp burn-in requires a configured re-encoding worker'
						: undefined,
					burnInTimestamp: command.burnInTimestamp
				});
				const fixture = exportCreateResults.shift();
				const exportJob = fixture ? applyExportFixture(base, fixture, exportFileHash) : base;
				exportJobs.set(exportJob.jobId, exportJob);
				return encodedOk(request.requestId, { case: 'exportJob', value: exportJob });
			}
			const current = exportJobs.get(action.value.jobId);
			if (!current) return encodedError(request.requestId, 'export job not found');
			if (action.case === 'get') {
				const fixture = exportGetResults.shift();
				const exportJob = fixture ? applyExportFixture(current, fixture, exportFileHash) : current;
				exportJobs.set(exportJob.jobId, exportJob);
				return encodedOk(request.requestId, { case: 'exportJob', value: exportJob });
			}
			if (action.case === 'cancel') {
				const exportJob = create(ExportJobSchema, {
					...current,
					status: ExportJobStatus.CANCELLED
				});
				exportJobs.set(exportJob.jobId, exportJob);
				return encodedOk(request.requestId, { case: 'exportJob', value: exportJob });
			}
			if (action.case === 'retry') {
				const exportJob = options.exportRetryResult
					? applyExportFixture(current, options.exportRetryResult, exportFileHash)
					: create(ExportJobSchema, {
							...current,
							status: ExportJobStatus.RUNNING,
							progressPerMille: 0,
							bytesWritten: 0n,
							error: undefined
						});
				exportJobs.set(exportJob.jobId, exportJob);
				return encodedOk(request.requestId, { case: 'exportJob', value: exportJob });
			}
			if (current.status !== ExportJobStatus.READY) {
				return encodedError(request.requestId, 'export job is not ready');
			}
			const chunkSize = Math.max(1, Math.ceil(exportFile.byteLength / 2));
			const chunkCount = Math.ceil(exportFile.byteLength / chunkSize);
			for (let chunkIndex = 0; chunkIndex < chunkCount; chunkIndex += 1) {
				pendingDataMessages.push(
					encodedData(
						create(MessageSchema, {
							message: {
								case: 'export',
								value: create(ExportMessageSchema, {
									message: {
										case: 'fileChunk',
										value: create(ExportFileChunkSchema, {
											jobId: current.jobId,
											chunkIndex,
											chunkCount,
											payload: exportFile.slice(
												chunkIndex * chunkSize,
												Math.min(exportFile.byteLength, (chunkIndex + 1) * chunkSize)
											)
										})
									}
								})
							}
						})
					)
				);
			}
			return encodedOk(request.requestId, {
				case: 'exportDownload',
				value: create(ExportDownloadResultSchema, {
					job: current,
					channel: DataChannelKind.RELIABLE_DATA,
					chunkCount
				})
			});
		}
		if (request.command.case === 'healthCommand' && request.command.value.action.case === 'get') {
			await options.healthGate;
			return encodedOk(request.requestId, {
				case: 'healthResult',
				value: protoHealthSnapshot(options.health ?? {})
			});
		}
		if (request.command.case === 'loggingCommand') {
			const action = request.command.value.action;
			if (action.case === 'setFilter') {
				requests.loggingFilters.push(action.value.filter);
				if (action.value.filter.includes('verbose')) {
					return encodedError(request.requestId, 'invalid log filter');
				}
				activeFilter = action.value.filter;
			} else if (action.case !== 'getSettings') {
				throw new Error(`unexpected logging action ${action.case}`);
			}
			return encodedOk(request.requestId, {
				case: 'loggingSettingsResult',
				value: create(LoggingSettingsResultSchema, {
					activeFilter,
					defaultFilter: 'info,keeppeek=debug',
					version: '0.1.0',
					buffer: create(LogBufferStatsSchema, {
						entryCount: 1n,
						byteCount: 256n,
						maxEntries: 10_000n,
						maxBytes: 8_388_608n,
						activeStreams: 1n,
						maxStreams: 8n
					})
				})
			});
		}
		if (
			request.command.case === 'serverCommand' &&
			request.command.value.action.case === 'restart'
		) {
			requests.restarts += 1;
			return encodedOk(request.requestId, {
				case: 'restartResult',
				value: create(RestartResultSchema, { restarting: true })
			});
		}
		if (
			request.command.case === 'cameraConfigurationCommand' &&
			request.command.value.action.case === 'discover'
		) {
			requests.discoverySubnets.push([...request.command.value.action.value.subnets]);
			await options.discoveryGate;
			return encodedOk(request.requestId, {
				case: 'cameraDiscoveryResult',
				value: create(CameraDiscoveryResultSchema, {
					cameras: (options.discoveredCameras ?? []).map((camera) =>
						create(DiscoveredCameraSchema, {
							ip: camera.ip,
							brand: camera.brand,
							name: camera.name ?? undefined,
							model: camera.model ?? undefined,
							onvifPort: camera.onvif_port ?? undefined,
							sources: camera.sources,
							configured: camera.configured,
							health: camera.health ?? undefined,
							catalog: camera.catalog ? protoCameraCatalogCamera(camera.catalog) : undefined
						})
					)
				})
			});
		}
		if (
			request.command.case === 'cameraConfigurationCommand' &&
			request.command.value.action.case === 'getCatalog'
		) {
			return encodedOk(request.requestId, {
				case: 'cameraCatalogInfo',
				value: protoCameraCatalogInfo(options.cameraCatalog ?? defaultCameraCatalog)
			});
		}
		if (
			request.command.case === 'cameraConfigurationCommand' &&
			request.command.value.action.case === 'searchCatalog'
		) {
			const search = request.command.value.action.value;
			requests.catalogSearches.push({ query: search.query, limit: search.limit, ip: search.ip });
			return encodedOk(request.requestId, {
				case: 'cameraCatalogSearchResult',
				value: create(CameraCatalogSearchResultSchema, {
					cameras: (options.cameraCatalogSearchResults ?? []).map((camera) =>
						protoCameraCatalogCamera(cameraCatalogCameraForSearch(camera, search.ip))
					)
				})
			});
		}
		if (
			request.command.case === 'cameraConfigurationCommand' &&
			request.command.value.action.case === 'probeStreams'
		) {
			const probe = request.command.value.action.value;
			requests.streamProbes.push({ ip: probe.ip, onvifPort: probe.onvifPort ?? null });
			await options.streamProbeGate;
			const result = options.streamProbeResult ?? {
				main_rtsp_url: `rtsp://${probe.ip}:554/onvif-main`,
				sub_rtsp_url: `rtsp://${probe.ip}:554/onvif-sub`,
				onvif_port: probe.onvifPort ?? 80
			};
			return encodedOk(request.requestId, {
				case: 'cameraStreamProbeResult',
				value: create(CameraStreamProbeResultSchema, {
					mainRtspUrl: result.main_rtsp_url ?? undefined,
					subRtspUrl: result.sub_rtsp_url ?? undefined,
					onvifPort: result.onvif_port ?? undefined
				})
			});
		}
		if (
			request.command.case === 'runtimeConfigurationCommand' &&
			request.command.value.action.case === 'get'
		) {
			if (!options.runtimeConfiguration) {
				throw new Error('runtime configuration result is not configured');
			}
			return encodedOk(request.requestId, {
				case: 'runtimeConfigurationResult',
				value: protoRuntimeResult({
					config: options.runtimeConfiguration,
					restart_required: false
				})
			});
		}
		if (
			request.command.case === 'runtimeConfigurationCommand' &&
			request.command.value.action.case === 'update'
		) {
			const update = runtimeUpdate(request.command.value.action.value);
			requests.runtimeUpdates.push(update);
			await options.runtimeUpdateGate;
			if (!options.runtimeUpdateResult) throw new Error('runtime update result is not configured');
			return encodedOk(request.requestId, {
				case: 'runtimeConfigurationResult',
				value: protoRuntimeResult(options.runtimeUpdateResult)
			});
		}
		if (
			request.command.case === 'cameraConfigurationCommand' &&
			request.command.value.action.case === 'get'
		) {
			return encodedOk(request.requestId, {
				case: 'cameraConfigurationResult',
				value: create(CameraConfigurationResultSchema, {
					cameras: (options.cameraSettings ?? []).map(protoCameraSettings)
				})
			});
		}
		if (
			request.command.case === 'cameraConfigurationCommand' &&
			request.command.value.action.case === 'update'
		) {
			const update = request.command.value.action.value;
			requests.cameraUpdates.push({ ip: update.ip, update: cameraUpdate(update) });
			if (options.cameraUpdateError) {
				return encodedError(request.requestId, options.cameraUpdateError);
			}
			if (!options.cameraUpdateResult) throw new Error('camera update result is not configured');
			return encodedOk(request.requestId, {
				case: 'cameraConfigurationResult',
				value: create(CameraConfigurationResultSchema, {
					camera: protoCameraSettings(options.cameraUpdateResult.camera),
					restartRequired: options.cameraUpdateResult.restart_required
				})
			});
		}
		if (
			request.command.case === 'cameraConfigurationCommand' &&
			request.command.value.action.case === 'remove'
		) {
			requests.removedCameraIps.push(request.command.value.action.value.ip);
			return encodedOk(request.requestId, {
				case: 'cameraConfigurationResult',
				value: create(CameraConfigurationResultSchema, { removed: true })
			});
		}
		if (request.command.case !== 'cameraControlCommand') {
			throw new Error(`unexpected command ${request.command.case}`);
		}
		const action = request.command.value.action;
		const result: Ok['result'] =
			action.case === 'ptz'
				? (() => {
						const ptzAction = action.value.action;
						if (ptzAction.case === undefined) throw new Error('PTZ action is empty');
						if (
							ptzAction.case !== 'continuous' &&
							ptzAction.case !== 'stop' &&
							ptzAction.case !== 'listPresets' &&
							ptzAction.case !== 'gotoPreset'
						) {
							throw new Error(`unsupported PTZ action ${ptzAction.case}`);
						}
						requests.ptz.push({
							sourceId: action.value.sourceId,
							action: ptzAction.case,
							...(ptzAction.case === 'continuous'
								? {
										pan: ptzAction.value.pan,
										tilt: ptzAction.value.tilt,
										zoom: ptzAction.value.zoom
									}
								: {}),
							...(ptzAction.case === 'gotoPreset' ? { presetId: ptzAction.value.presetId } : {})
						});
						return {
							case: 'ptzResult' as const,
							value: create(PtzResultSchema, {
								sourceId: action.value.sourceId,
								presets:
									ptzAction.case === 'listPresets'
										? (options.ptzPresets ?? []).map((preset) =>
												create(PtzPresetSchema, {
													presetId: preset.id,
													name: preset.name
												})
											)
										: []
							})
						};
					})()
				: action.case === 'getMotionDetection'
					? {
							case: 'motionDetectionResult' as const,
							value: create(MotionDetectionResultSchema, {
								supported: options.motionDetection?.supported ?? false,
								controllable: options.motionDetection?.controllable ?? false,
								enabled: options.motionDetection?.enabled ?? undefined,
								error: options.motionDetection?.error ?? undefined
							})
						}
					: action.case === 'setMotionDetection'
						? (() => {
								requests.motion.push({
									sourceId: action.value.sourceId,
									enabled: action.value.enabled
								});
								return {
									case: 'motionDetectionResult' as const,
									value: create(MotionDetectionResultSchema, {
										supported: true,
										controllable: true,
										enabled: action.value.enabled
									})
								};
							})()
						: action.case === 'setManufacturer'
							? (() => {
									const update = action.value.manufacturer?.value;
									if (!update || update.case === undefined) {
										throw new Error('manufacturer update is empty');
									}
									const manufacturer = update.case === 'set' ? update.value : null;
									requests.manufacturer.push({ sourceId: action.value.sourceId, manufacturer });
									return {
										case: 'cameraManufacturerResult' as const,
										value: create(CameraManufacturerResultSchema, {
											sourceId: action.value.sourceId,
											manufacturer: manufacturer ?? reportedManufacturer
										})
									};
								})()
							: (() => {
									throw new Error(`unexpected camera action ${action.case}`);
								})();
		return encodedOk(request.requestId, result);
	});
	await page.addInitScript(() => {
		type ControlWindow = Window & {
			handleKeepPeekControl(payload: number[]): Promise<number[]>;
			takeKeepPeekData(): Promise<number[][]>;
			getKeepPeekCapabilities(): Promise<number[]>;
		};

		class MockDataChannel {
			readyState: RTCDataChannelState = 'connecting';
			binaryType: BinaryType = 'blob';
			onopen: RTCDataChannel['onopen'] = null;
			onclose: RTCDataChannel['onclose'] = null;
			onerror: RTCDataChannel['onerror'] = null;
			onmessage: RTCDataChannel['onmessage'] = null;

			constructor(
				readonly label: string,
				private deliver: (label: string, payload: ArrayBuffer) => void
			) {}

			send(data: string | Blob | ArrayBuffer | ArrayBufferView): void {
				if (this.label !== 'control-channel' || typeof data === 'string' || data instanceof Blob) {
					return;
				}
				const bytes =
					data instanceof ArrayBuffer
						? new Uint8Array(data)
						: new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
				void (window as unknown as ControlWindow)
					.handleKeepPeekControl(Array.from(bytes))
					.then(async (response) => {
						const payload = Uint8Array.from(response).buffer;
						this.receive(payload);
						const messages = await (window as unknown as ControlWindow).takeKeepPeekData();
						for (const message of messages) {
							this.deliver('reliable-data', Uint8Array.from(message).buffer);
						}
					});
			}

			receive(payload: ArrayBuffer): void {
				this.onmessage?.call(
					this as unknown as RTCDataChannel,
					new MessageEvent('message', { data: payload })
				);
			}

			open(): void {
				this.readyState = 'open';
				this.onopen?.call(this as unknown as RTCDataChannel, new Event('open'));
				if (this.label === 'control-channel') {
					void (window as unknown as ControlWindow).getKeepPeekCapabilities().then((message) => {
						this.receive(Uint8Array.from(message).buffer);
					});
				}
			}

			close(): void {
				this.readyState = 'closed';
			}
		}

		class MockPeerConnection {
			localDescription: RTCSessionDescriptionInit | null = null;
			iceGatheringState: RTCIceGatheringState = 'complete';
			connectionState: RTCPeerConnectionState = 'new';
			iceConnectionState: RTCIceConnectionState = 'new';
			ontrack: RTCPeerConnection['ontrack'] = null;
			onconnectionstatechange: RTCPeerConnection['onconnectionstatechange'] = null;
			oniceconnectionstatechange: RTCPeerConnection['oniceconnectionstatechange'] = null;
			private channels: MockDataChannel[] = [];
			private transceivers: RTCRtpTransceiver[] = [];

			createDataChannel(label: string): RTCDataChannel {
				const channel = new MockDataChannel(label, (target, payload) => {
					this.channels.find((candidate) => candidate.label === target)?.receive(payload);
				});
				this.channels.push(channel);
				return channel as unknown as RTCDataChannel;
			}

			addTransceiver(): RTCRtpTransceiver {
				const transceiver = {
					mid: null,
					setCodecPreferences() {}
				} as unknown as RTCRtpTransceiver;
				this.transceivers.push(transceiver);
				return transceiver;
			}

			async createOffer(): Promise<RTCSessionDescriptionInit> {
				this.transceivers.forEach((transceiver, index) => {
					(transceiver as unknown as { mid: string }).mid = String(index);
				});
				return { type: 'offer', sdp: 'v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel' };
			}

			async setLocalDescription(description: RTCSessionDescriptionInit): Promise<void> {
				this.localDescription = description;
			}

			async setRemoteDescription(): Promise<void> {
				this.connectionState = 'connected';
				this.iceConnectionState = 'connected';
				for (const channel of this.channels) channel.open();
			}

			close(): void {
				this.connectionState = 'closed';
				for (const channel of this.channels) channel.close();
			}
		}

		Object.defineProperty(window, 'RTCPeerConnection', { value: MockPeerConnection });
		Object.defineProperty(navigator, 'sendBeacon', { value: () => true });
	});
	await page.route('**/create', async (route) => {
		await route.fulfill({
			json: { session_id: 'playwright-control', answer: { type: 'answer', sdp: 'v=0' } }
		});
	});
	await page.route('**/delete', async (route) => {
		await route.fulfill({ status: 204 });
	});
	return requests;
}

function encodedCapabilities(
	cameras: readonly CameraListItem[],
	storedRanges: readonly StoredRangeFixture[],
	capabilityIds: readonly string[]
): number[] {
	const envelope = create(ControlEnvelopeSchema, {
		message: {
			case: 'notification',
			value: create(NotificationSchema, {
				event: {
					case: 'initialCapabilities',
					value: create(ServerCapabilitiesSchema, {
						revision: 1n,
						selfSourceSessionId: 'webrtc-client-playwright',
						capabilityIds: [...capabilityIds],
						cameras: cameras.map((camera) =>
							create(CameraInfoSchema, {
								sourceId: camera.id,
								displayName: camera.name ?? camera.id,
								manufacturer: camera.manufacturer ?? undefined,
								model: camera.model ?? undefined,
								firmwareVersion: camera.firmware_version ?? undefined,
								serialNumber: camera.serial_number ?? undefined,
								hardwareId: camera.hardware_id ?? undefined,
								ip: camera.ip,
								hostname: camera.hostname ?? undefined,
								macAddress: camera.mac_address ?? undefined,
								webUrl: camera.web_url,
								httpPort: camera.ports?.http ?? undefined,
								httpsPort: camera.ports?.https ?? undefined,
								rtspPort: camera.ports?.rtsp ?? undefined,
								onvifPort: camera.ports?.onvif ?? undefined,
								isReolink: camera.is_reolink,
								deviceCapabilities: create(CameraDeviceCapabilitiesSchema, {
									audio: camera.capabilities?.audio ?? false,
									events: camera.capabilities?.events ?? false,
									recording: camera.capabilities?.recording ?? false,
									analytics: camera.capabilities?.analytics ?? false,
									imaging: camera.capabilities?.imaging ?? false,
									twoWayAudio: camera.capabilities?.two_way_audio ?? false
								}),
								ptz: create(PtzCapabilitySchema, {
									supported: camera.capabilities?.ptz ?? false
								})
							})
						),
						sourceSessions: cameras
							.filter((camera) => camera.profiles.some((profile) => profile.encoding))
							.map((camera) =>
								create(SourceSessionSchema, {
									sourceSessionId: `camera:${camera.id}`,
									sourceId: camera.id,
									displayName: camera.name ?? camera.id,
									video: create(MediaStreamCapabilitySchema, {
										variants: camera.profiles
											.filter((profile) => profile.encoding)
											.map((profile) =>
												create(MediaVariantCapabilitySchema, {
													variantId: profile.stream,
													codec: create(CodecDescriptorSchema, {
														name: profile.encoding ?? ''
													}),
													format: create(MediaDataFormatSchema, {
														format: {
															case: 'video',
															value: create(VideoDataFormatSchema, resolution(profile.resolution))
														}
													}),
													deliveryTransports: [DeliveryTransport.RTP],
													nominalBitrateBps: BigInt(profile.bitrate_kbps ?? 0) * 1_000n
												})
											)
									})
								})
							),
						storedMediaSources: cameras.map((camera) => {
							const streamIds = new Set(
								storedRanges
									.filter((range) => range.sourceId === camera.id)
									.map((range) => range.streamId)
							);
							for (const profile of camera.profiles) streamIds.add(profile.stream);
							return create(StoredMediaSourceCapabilitySchema, {
								sourceId: camera.id,
								displayName: camera.name ?? camera.id,
								streams: [...streamIds].map((streamId) =>
									create(StoredMediaStreamCapabilitySchema, {
										streamId,
										contentType: 'video/mp4',
										deliveryChannels: [DataChannelKind.RELIABLE_DATA]
									})
								)
							});
						})
					})
				}
			})
		}
	});
	return Array.from(toBinary(ControlEnvelopeSchema, envelope));
}

function fixtureExportJob(fixture: ExportJobFixture, sha256: string): ExportJob {
	const startMs = fixture.requestedStartMs ?? Date.parse('2026-08-18T06:11:48Z');
	const endMs = fixture.requestedEndMs ?? Date.parse('2026-08-18T06:14:20Z');
	const base = create(ExportJobSchema, {
		jobId: fixture.jobId ?? 'export-fixture',
		sourceId: fixture.sourceId ?? 'front-door',
		streamId: fixture.streamId ?? 'main',
		requestedStartTime: timestampFromDate(new Date(startMs)),
		requestedEndTime: timestampFromDate(new Date(endMs)),
		status: exportJobStatus(fixture.status)
	});
	return applyExportFixture(base, fixture, sha256);
}

function applyExportFixture(base: ExportJob, fixture: ExportJobFixture, sha256: string): ExportJob {
	const status = exportJobStatus(fixture.status);
	return create(ExportJobSchema, {
		...base,
		jobId: fixture.jobId ?? base.jobId,
		sourceId: fixture.sourceId ?? base.sourceId,
		streamId: fixture.streamId ?? base.streamId,
		requestedStartTime:
			fixture.requestedStartMs === undefined
				? base.requestedStartTime
				: timestampFromDate(new Date(fixture.requestedStartMs)),
		requestedEndTime:
			fixture.requestedEndMs === undefined
				? base.requestedEndTime
				: timestampFromDate(new Date(fixture.requestedEndMs)),
		alignedStartTime:
			fixture.alignedStartMs === undefined
				? base.alignedStartTime
				: timestampFromDate(new Date(fixture.alignedStartMs)),
		status,
		progressPerMille:
			fixture.progress === undefined
				? status === ExportJobStatus.READY
					? 1_000
					: base.progressPerMille
				: Math.round(fixture.progress * 1_000),
		bytesWritten:
			fixture.bytesWritten === undefined ? base.bytesWritten : BigInt(fixture.bytesWritten),
		estimatedBytes:
			fixture.estimatedBytes === undefined ? base.estimatedBytes : BigInt(fixture.estimatedBytes),
		fileName:
			fixture.fileName ??
			(status === ExportJobStatus.READY
				? 'front-door_2026-08-18T06-11-48Z_152s.mp4'
				: base.fileName),
		sha256: fixture.sha256 ?? (status === ExportJobStatus.READY ? sha256 : base.sha256),
		expiresAt:
			fixture.expiresAtMs === undefined
				? status === ExportJobStatus.READY
					? timestampFromDate(new Date(Date.now() + 24 * 60 * 60_000))
					: base.expiresAt
				: timestampFromDate(new Date(fixture.expiresAtMs)),
		missingRanges:
			fixture.missingRanges?.map((missing) =>
				create(ExportMissingRangeSchema, {
					startTime: timestampFromDate(new Date(missing.startMs)),
					endTime: timestampFromDate(new Date(missing.endMs))
				})
			) ?? base.missingRanges,
		error: fixture.error ?? base.error,
		retryable: fixture.retryable ?? base.retryable,
		burnInTimestamp: fixture.burnInTimestamp ?? base.burnInTimestamp
	});
}

function exportJobStatus(status: ExportJobFixture['status']): ExportJobStatus {
	return status === 'running'
		? ExportJobStatus.RUNNING
		: status === 'partial'
			? ExportJobStatus.PARTIAL
			: status === 'ready'
				? ExportJobStatus.READY
				: status === 'failed'
					? ExportJobStatus.FAILED
					: status === 'cancelled'
						? ExportJobStatus.CANCELLED
						: ExportJobStatus.EXPIRED;
}

function resolution(value: string | null): { width: number; height: number } {
	const [width, height] = value?.split('x').map(Number) ?? [];
	return {
		width: Number.isSafeInteger(width) ? width : 0,
		height: Number.isSafeInteger(height) ? height : 0
	};
}

function protoHealthSnapshot(health: HealthFixture) {
	const totals = health.totals ?? {};
	const system = health.system ?? {};
	const process = system.process ?? {};
	const memory = system.memory ?? {};
	const load = system.load ?? {};
	const storage = health.storage ?? {};
	const demand = storage.demand ?? {};
	const webrtc = health.webrtc ?? {};
	return create(ServerHealthSnapshotSchema, {
		status: health.status ?? 'healthy',
		generatedAtMs: fixtureBigInt(health.generated_at_ms),
		uptimeSeconds: fixtureBigInt(health.uptime_seconds),
		version: health.version ?? 'test',
		totals: create(HealthTotalsSnapshotSchema, {
			configuredCameras: fixtureBigInt(totals.configured_cameras),
			reportingCameras: fixtureBigInt(totals.reporting_cameras),
			configuredVideoStreams: fixtureBigInt(totals.configured_video_streams),
			reportingVideoStreams: fixtureBigInt(totals.reporting_video_streams),
			ingressFps: totals.ingress_fps ?? 0,
			ingressBitrateBps: fixtureBigInt(totals.ingress_bitrate_bps),
			frames: fixtureBigInt(totals.frames),
			keyframes: fixtureBigInt(totals.keyframes),
			drops: fixtureBigInt(totals.drops),
			errors: fixtureBigInt(totals.errors),
			reconnects: fixtureBigInt(totals.reconnects)
		}),
		system: create(SystemHealthSnapshotSchema, {
			hostName: system.host_name ?? undefined,
			osName: system.os_name ?? undefined,
			osVersion: system.os_version ?? undefined,
			kernelVersion: system.kernel_version ?? undefined,
			architecture: system.architecture ?? 'test',
			systemUptimeSeconds: fixtureBigInt(system.system_uptime_seconds),
			bootTimeSeconds: fixtureBigInt(system.boot_time_seconds),
			logicalCores: fixtureBigInt(system.logical_cores),
			physicalCores: optionalFixtureBigInt(system.physical_cores),
			cpuBrand: system.cpu_brand ?? undefined,
			systemCpuPercent: system.system_cpu_percent ?? 0,
			process: create(ProcessHealthSnapshotSchema, {
				pid: process.pid ?? 0,
				name: process.name ?? undefined,
				executable: process.executable ?? undefined,
				workingDirectory: process.working_directory ?? undefined,
				cpuPercent: process.cpu_percent ?? undefined,
				cpuCapacityPercent: process.cpu_capacity_percent ?? undefined,
				cpuCoreEquivalents: process.cpu_core_equivalents ?? undefined,
				residentMemoryBytes: optionalFixtureBigInt(process.resident_memory_bytes),
				memoryCapacityPercent: process.memory_capacity_percent ?? undefined,
				virtualMemoryBytes: optionalFixtureBigInt(process.virtual_memory_bytes),
				startedAtSeconds: optionalFixtureBigInt(process.started_at_seconds),
				uptimeSeconds: optionalFixtureBigInt(process.uptime_seconds),
				tasks: optionalFixtureBigInt(process.tasks),
				readBytesPerSecond: optionalFixtureBigInt(process.read_bytes_per_second),
				writeBytesPerSecond: optionalFixtureBigInt(process.write_bytes_per_second),
				totalReadBytes: optionalFixtureBigInt(process.total_read_bytes),
				totalWrittenBytes: optionalFixtureBigInt(process.total_written_bytes)
			}),
			memory: create(MemoryHealthSnapshotSchema, {
				totalBytes: fixtureBigInt(memory.total_bytes),
				usedBytes: fixtureBigInt(memory.used_bytes),
				availableBytes: fixtureBigInt(memory.available_bytes),
				totalSwapBytes: fixtureBigInt(memory.total_swap_bytes),
				usedSwapBytes: fixtureBigInt(memory.used_swap_bytes)
			}),
			load: create(LoadHealthSnapshotSchema, {
				oneMinute: load.one_minute ?? 0,
				fiveMinutes: load.five_minutes ?? 0,
				fifteenMinutes: load.fifteen_minutes ?? 0
			}),
			cpus: (system.cpus ?? []).map((cpu) =>
				create(CpuHealthSnapshotSchema, {
					name: cpu.name ?? '',
					usagePercent: cpu.usage_percent ?? 0,
					frequencyMhz: fixtureBigInt(cpu.frequency_mhz)
				})
			),
			networkEgressBps: fixtureBigInt(system.network_egress_bps),
			networks: (system.networks ?? []).map((network) =>
				create(NetworkHealthSnapshotSchema, {
					name: network.name ?? '',
					receivedBytesPerSecond: fixtureBigInt(network.received_bytes_per_second),
					transmittedBytesPerSecond: fixtureBigInt(network.transmitted_bytes_per_second),
					receivedPacketsPerSecond: fixtureBigInt(network.received_packets_per_second),
					transmittedPacketsPerSecond: fixtureBigInt(network.transmitted_packets_per_second),
					receiveErrors: fixtureBigInt(network.receive_errors),
					transmitErrors: fixtureBigInt(network.transmit_errors),
					totalReceivedBytes: fixtureBigInt(network.total_received_bytes),
					totalTransmittedBytes: fixtureBigInt(network.total_transmitted_bytes)
				})
			),
			disks: (system.disks ?? []).map((disk) =>
				create(DiskHealthSnapshotSchema, {
					name: disk.name ?? '',
					kind: disk.kind ?? '',
					fileSystem: disk.file_system ?? '',
					mountPoint: disk.mount_point ?? '',
					totalBytes: fixtureBigInt(disk.total_bytes),
					availableBytes: fixtureBigInt(disk.available_bytes),
					usedBytes: fixtureBigInt(disk.used_bytes),
					removable: disk.removable ?? false,
					storesRecordings: disk.stores_recordings ?? false
				})
			),
			temperatures: (system.temperatures ?? []).map((temperature) =>
				create(TemperatureHealthSnapshotSchema, {
					label: temperature.label ?? '',
					currentCelsius: temperature.current_celsius ?? undefined,
					maxCelsius: temperature.max_celsius ?? undefined,
					criticalCelsius: temperature.critical_celsius ?? undefined
				})
			)
		}),
		storage: create(StorageHealthSnapshotSchema, {
			mediumTermPath: storage.medium_term_path ?? '',
			longTermPath: storage.long_term_path ?? '',
			pathsAreSame: storage.paths_are_same ?? false,
			shortTermSeconds: fixtureBigInt(storage.short_term_seconds),
			mediumTermSeconds: fixtureBigInt(storage.medium_term_seconds),
			flushIntervalSeconds: fixtureBigInt(storage.flush_interval_seconds),
			writeBufferBytes: fixtureBigInt(storage.write_buffer_bytes),
			longTermMaxBytes: fixtureBigInt(storage.long_term_max_bytes),
			catalogBytes: optionalFixtureBigInt(storage.catalog_bytes),
			catalog: storage.catalog
				? create(CatalogHealthSnapshotSchema, {
						recordingFiles: fixtureBigInt(storage.catalog.recording_files),
						finalizedFiles: fixtureBigInt(storage.catalog.finalized_files),
						activeFiles: fixtureBigInt(storage.catalog.active_files),
						fragments: fixtureBigInt(storage.catalog.fragments),
						fragmentBytes: fixtureBigInt(storage.catalog.fragment_bytes),
						events: fixtureBigInt(storage.catalog.events),
						openEvents: fixtureBigInt(storage.catalog.open_events),
						eventThumbnails: fixtureBigInt(storage.catalog.event_thumbnails)
					})
				: undefined,
			demand: create(RecordingDemandHealthSnapshotSchema, {
				activeStreams: fixtureBigInt(demand.active_streams),
				totalViewers: fixtureBigInt(demand.total_viewers),
				leasedStreams: fixtureBigInt(demand.leased_streams),
				streams: (demand.streams ?? []).map((stream) =>
					create(RecordingDemandStreamHealthSnapshotSchema, {
						streamId: stream.stream_id ?? '',
						viewers: fixtureBigInt(stream.viewers),
						leaseRemainingMs: optionalFixtureBigInt(stream.lease_remaining_ms)
					})
				)
			})
		}),
		webrtc: create(WebRtcHealthSnapshotSchema, {
			activeSessions: fixtureBigInt(webrtc.active_sessions),
			adaptiveSessions: fixtureBigInt(webrtc.adaptive_sessions),
			multiTrackSessions: fixtureBigInt(webrtc.browser_sessions),
			multiTracks: fixtureBigInt(webrtc.browser_tracks),
			fixedSessions: fixtureBigInt(webrtc.fixed_sessions),
			activeMain: fixtureBigInt(webrtc.active_main),
			activeSub: fixtureBigInt(webrtc.active_sub),
			requestedAuto: fixtureBigInt(webrtc.requested_auto),
			requestedHigh: fixtureBigInt(webrtc.requested_high),
			requestedLow: fixtureBigInt(webrtc.requested_low),
			estimatedBitrateMinBps: optionalFixtureBigInt(webrtc.estimated_bitrate_min_bps),
			estimatedBitrateAvgBps: optionalFixtureBigInt(webrtc.estimated_bitrate_avg_bps),
			estimatedBitrateMaxBps: optionalFixtureBigInt(webrtc.estimated_bitrate_max_bps),
			sourceBitrateBps: fixtureBigInt(webrtc.source_bitrate_bps),
			publishedFrames: fixtureBigInt(webrtc.published_frames),
			publishedBytes: fixtureBigInt(webrtc.published_bytes),
			deliveredFrames: fixtureBigInt(webrtc.delivered_frames),
			writtenFrames: fixtureBigInt(webrtc.written_frames),
			queueCapacity: fixtureBigInt(webrtc.queue_capacity),
			queuedFrames: fixtureBigInt(webrtc.queued_frames),
			queueDepthMax: fixtureBigInt(webrtc.queue_depth_max),
			queueHighWater: fixtureBigInt(webrtc.queue_high_water),
			queueDrops: fixtureBigInt(webrtc.queue_drops),
			queueDiscardedFrames: fixtureBigInt(webrtc.queue_discarded_frames),
			queueRecoveryDrops: fixtureBigInt(webrtc.queue_recovery_drops),
			sessionQueues: (webrtc.session_queues ?? []).map((queue) =>
				create(WebRtcSessionQueueHealthSnapshotSchema, {
					sessionId: fixtureBigInt(queue.session_id),
					trackId: queue.track_id ?? undefined,
					cameraIp: queue.camera_ip ?? '',
					stream: queue.stream ?? 'main',
					depth: fixtureBigInt(queue.depth),
					highWater: fixtureBigInt(queue.high_water),
					writtenFrames: fixtureBigInt(queue.written_frames),
					fullDrops: fixtureBigInt(queue.full_drops),
					discardedFrames: fixtureBigInt(queue.discarded_frames),
					recoveryDrops: fixtureBigInt(queue.recovery_drops)
				})
			),
			sources: (webrtc.sources ?? []).map((source) =>
				create(WebRtcSourceHealthSnapshotSchema, {
					cameraIp: source.camera_ip ?? '',
					stream: source.stream ?? 'main',
					subscribers: fixtureBigInt(source.subscribers),
					bitrateBps: optionalFixtureBigInt(source.bitrate_bps),
					hasKeyframe: source.has_keyframe ?? false,
					keyframeAgeMs: optionalFixtureBigInt(source.keyframe_age_ms)
				})
			)
		}),
		cameras: (health.cameras ?? []).map((camera) =>
			create(CameraHealthSnapshotSchema, {
				id: camera.id ?? '',
				ip: camera.ip ?? '',
				name: camera.name ?? camera.id ?? '',
				manufacturer: camera.manufacturer ?? undefined,
				model: camera.model ?? undefined,
				firmwareVersion: camera.firmware_version ?? undefined,
				backend: camera.backend ?? '',
				transport: camera.transport ?? '',
				state: camera.state ?? 'starting',
				lifecycle: camera.lifecycle ?? undefined,
				lastError: camera.last_error ?? undefined,
				configuredProfiles: (camera.configured_profiles ?? []).map((profile) =>
					create(HealthProfileSummarySchema, {
						name: profile.name ?? '',
						stream: profile.stream ?? 'main',
						encoding: profile.encoding ?? undefined,
						resolution: profile.resolution ?? undefined,
						framerate: profile.framerate ?? undefined,
						bitrateKbps: profile.bitrate_kbps ?? undefined,
						gop: profile.gop ?? undefined,
						h264Profile: profile.h264_profile ?? undefined,
						audio: profile.audio
							? create(HealthAudioProfileSummarySchema, {
									encoding: profile.audio.encoding ?? '',
									sampleRate: profile.audio.sample_rate ?? undefined,
									bitrateKbps: profile.audio.bitrate_kbps ?? undefined
								})
							: undefined
					})
				),
				streams: (camera.streams ?? []).map((stream) =>
					create(StreamHealthSnapshotSchema, {
						type: stream.type ?? '',
						codec: stream.codec,
						resolution: stream.resolution,
						fps: stream.fps,
						expectedFps: stream.expected_fps,
						kfFps: stream.kf_fps,
						kbps: stream.kbps,
						maxFrameKb: stream.max_frame_kb,
						gapMinMs: stream.gap_min_ms,
						gapAvgMs: stream.gap_avg_ms,
						gapMaxMs: stream.gap_max_ms,
						jitterSamples: optionalFixtureBigInt(stream.jitter_samples),
						jitterP50Ms: stream.jitter_p50_ms,
						jitterP99Ms: stream.jitter_p99_ms,
						frames: optionalFixtureBigInt(stream.frames),
						bytes: optionalFixtureBigInt(stream.bytes),
						keyframes: optionalFixtureBigInt(stream.keyframes),
						reconnects: optionalFixtureBigInt(stream.reconnects),
						drops: optionalFixtureBigInt(stream.drops),
						errors: optionalFixtureBigInt(stream.errors),
						updatedAtMs: fixtureBigInt(stream.updated_at_ms),
						reportAgeMs: fixtureBigInt(stream.report_age_ms)
					})
				)
			})
		),
		issues: (health.issues ?? []).map((issue) =>
			create(HealthIssueSnapshotSchema, {
				severity: issue.severity ?? 'info',
				scope: issue.scope ?? '',
				message: issue.message ?? ''
			})
		)
	});
}

function fixtureBigInt(value: number | undefined): bigint {
	return BigInt(Math.max(0, Math.trunc(value ?? 0)));
}

function optionalFixtureBigInt(value: number | null | undefined): bigint | undefined {
	return value === null || value === undefined ? undefined : fixtureBigInt(value);
}

function timestampFromProto(timestamp: import('@bufbuild/protobuf/wkt').Timestamp): number {
	return Number(timestamp.seconds) * 1_000 + Math.trunc(timestamp.nanos / 1_000_000);
}

function protoEvent(fixture: StoredEventFixture) {
	const event = fixture.event;
	return create(EventSchema, {
		eventId: event.id,
		revision: 1n,
		sourceId: fixture.sourceId,
		mediaKind: MediaKind.VIDEO,
		origin: event.source === 'keeppeek' ? EventOrigin.KEEPPEEK : EventOrigin.CAMERA,
		eventType: event.kind,
		startTime: timestampFromDate(new Date(event.start_time_ms)),
		endTime:
			event.end_time_ms === null ? undefined : timestampFromDate(new Date(event.end_time_ms)),
		confidence: event.confidence ?? undefined,
		boundingBox: event.bbox
			? create(EventBoundingBoxSchema, {
					x: event.bbox[0],
					y: event.bbox[1],
					width: event.bbox[2],
					height: event.bbox[3]
				})
			: undefined,
		zone: event.zone ?? undefined,
		attachments: fixture.thumbnail
			? [
					create(EventAttachmentDescriptorSchema, {
						attachmentId: 'thumbnail',
						attachmentType: 'thumbnail',
						contentType: 'image/jpeg',
						byteLen: BigInt(fixture.thumbnail.byteLength),
						ordinal: 0,
						timestamp: timestampFromDate(new Date(event.start_time_ms))
					})
				]
			: []
	});
}

function cameraUpdate(
	update: import('../../src/lib/proto/webrtc_pb').UpdateCameraConfiguration
): CameraSettingsUpdate {
	const result: CameraSettingsUpdate = {};
	applyStringPatch(result, 'display_name', update.displayName);
	applyStringPatch(result, 'manufacturer', update.manufacturer);
	if (update.username !== undefined) result.username = update.username;
	if (update.password !== undefined) result.password = update.password;
	applyNumberPatch(result, 'onvif_port', update.onvifPort);
	applyNumberPatch(result, 'http_port', update.httpPort);
	applyStringPatch(result, 'main_rtsp_url', update.mainRtspUrl);
	applyStringPatch(result, 'sub_rtsp_url', update.subRtspUrl);
	applyStringPatch(result, 'uid', update.uid);
	if (update.backend !== undefined) {
		result.backend =
			update.backend === ProtoCameraBackend.RETINA
				? 'retina'
				: update.backend === ProtoCameraBackend.REO_PROTO
					? 'reo-proto'
					: 'auto';
	}
	if (update.transport !== undefined) {
		result.transport = update.transport === ProtoCameraTransport.UDP ? 'udp' : 'tcp';
	}
	return result;
}

function applyStringPatch(
	result: CameraSettingsUpdate,
	key: 'display_name' | 'manufacturer' | 'main_rtsp_url' | 'sub_rtsp_url' | 'uid',
	update: import('../../src/lib/proto/webrtc_pb').OptionalStringUpdate | undefined
): void {
	if (!update || update.value.case === undefined) return;
	result[key] = update.value.case === 'set' ? update.value.value : null;
}

function applyNumberPatch(
	result: CameraSettingsUpdate,
	key: 'onvif_port' | 'http_port',
	update: import('../../src/lib/proto/webrtc_pb').OptionalUint32Update | undefined
): void {
	if (!update || update.value.case === undefined) return;
	result[key] = update.value.case === 'set' ? update.value.value : null;
}

function protoCameraSettings(camera: CameraSettings) {
	return create(CameraSettingsSchema, {
		id: camera.id,
		ip: camera.ip,
		displayName: camera.display_name ?? undefined,
		manufacturerOverride: camera.manufacturer_override ?? undefined,
		usernameConfigured: camera.username_configured,
		passwordConfigured: camera.password_configured,
		onvifPort: camera.onvif_port ?? undefined,
		httpPort: camera.http_port ?? undefined,
		mainRtspUrl: camera.main_rtsp_url ?? undefined,
		subRtspUrl: camera.sub_rtsp_url ?? undefined,
		uidConfigured: camera.uid_configured,
		backend:
			camera.backend === 'retina'
				? ProtoCameraBackend.RETINA
				: camera.backend === 'reo-proto'
					? ProtoCameraBackend.REO_PROTO
					: ProtoCameraBackend.AUTO,
		transport: camera.transport === 'udp' ? ProtoCameraTransport.UDP : ProtoCameraTransport.TCP,
		health: camera.health ?? undefined,
		model: camera.model ?? undefined
	});
}

function protoCameraCatalogInfo(catalog: CameraCatalogInfo) {
	return create(CameraCatalogInfoSchema, {
		version: catalog.version,
		tag: catalog.tag,
		generatedAt: catalog.generated_at,
		cameraCount: catalog.camera_count,
		websiteUrl: catalog.website_url
	});
}

function cameraCatalogCameraForSearch(
	camera: CameraCatalogCamera,
	ip: string | undefined
): CameraCatalogCamera {
	if (!ip || !camera.stream_hints) return camera;
	return {
		...camera,
		stream_hints: {
			main_rtsp_url: catalogStreamHintForIp(camera.stream_hints.main_rtsp_url, ip),
			sub_rtsp_url: catalogStreamHintForIp(camera.stream_hints.sub_rtsp_url, ip)
		}
	};
}

function catalogStreamHintForIp(value: string | null, ip: string): string | null {
	if (!value) return null;
	try {
		const url = new URL(value);
		url.hostname = ip;
		return url.toString();
	} catch {
		return value;
	}
}

function protoCameraCatalogCamera(camera: CameraCatalogCamera) {
	return create(CameraCatalogCameraSchema, {
		id: camera.id,
		brand: camera.brand,
		model: camera.model,
		aliases: camera.aliases,
		cameraType: camera.camera_type,
		resolutionLabel: camera.resolution_label ?? undefined,
		megapixels: camera.megapixels ?? undefined,
		sensor: camera.sensor ?? undefined,
		fieldOfView: camera.field_of_view ?? undefined,
		nightVision: camera.night_vision ?? undefined,
		ipRating: camera.ip_rating ?? undefined,
		ikRating: camera.ik_rating ?? undefined,
		twoWayAudio: camera.two_way_audio ?? undefined,
		releaseYear: camera.release_year ?? undefined,
		communityNotesCount: camera.community_notes_count,
		protocols: camera.protocols,
		codecs: camera.codecs,
		streams: camera.streams.map((stream) =>
			create(CameraCatalogStreamSchema, {
				name: stream.name,
				resolution: stream.resolution ?? undefined,
				fps: stream.fps ?? undefined,
				codec: stream.codec ?? undefined
			})
		),
		sources: camera.sources,
		streamHints: camera.stream_hints
			? create(CameraCatalogStreamHintsSchema, {
					mainRtspUrl: camera.stream_hints.main_rtsp_url ?? undefined,
					subRtspUrl: camera.stream_hints.sub_rtsp_url ?? undefined
				})
			: undefined
	});
}

function runtimeUpdate(
	update: import('../../src/lib/proto/webrtc_pb').UpdateRuntimeConfiguration
): SettingsConfigUpdate {
	if (!update.storage) throw new Error('runtime storage is absent');
	return {
		host: update.host,
		port: update.port,
		move_existing_recordings: update.moveExistingRecordings,
		storage: {
			medium_term_path: update.storage.mediumTermPath,
			long_term_path: update.storage.longTermPath,
			recording_catalog_path: update.storage.recordingCatalogPath,
			event_thumbnail_path: update.storage.eventThumbnailPath,
			event_thumbnail_max_mb: Number(update.storage.eventThumbnailMaxMb),
			short_term_secs: Number(update.storage.shortTermSecs),
			medium_term_secs: Number(update.storage.mediumTermSecs),
			flush_interval_secs: Number(update.storage.flushIntervalSecs),
			write_buffer_bytes: Number(update.storage.writeBufferBytes),
			long_term_max_gb: Number(update.storage.longTermMaxGb)
		}
	};
}

function protoRuntimeResult(result: SettingsConfigUpdateResponse) {
	const config = result.config;
	return create(RuntimeConfigurationResultSchema, {
		config: create(SanitizedRuntimeConfigurationSchema, {
			host: config.host,
			port: config.port,
			storage: create(RuntimeStorageConfigurationSchema, {
				mediumTermPath: config.storage.medium_term_path,
				longTermPath: config.storage.long_term_path,
				recordingCatalogPath: config.storage.recording_catalog_path,
				eventThumbnailPath: config.storage.event_thumbnail_path,
				eventThumbnailMaxMb: BigInt(config.storage.event_thumbnail_max_mb),
				shortTermSecs: BigInt(config.storage.short_term_secs),
				mediumTermSecs: BigInt(config.storage.medium_term_secs),
				flushIntervalSecs: BigInt(config.storage.flush_interval_secs),
				writeBufferBytes: BigInt(config.storage.write_buffer_bytes),
				longTermMaxGb: BigInt(config.storage.long_term_max_gb)
			}),
			cameraCount: BigInt(config.camera_count),
			recordingEstimate: create(RecordingCapacityEstimateSchema, {
				estimatedBitrateBps: BigInt(config.recording_estimate.estimated_bitrate_bps),
				bytesPerDay: BigInt(config.recording_estimate.bytes_per_day),
				knownStreams: BigInt(config.recording_estimate.known_streams),
				unknownStreams: BigInt(config.recording_estimate.unknown_streams),
				estimatedRetentionDays: config.recording_estimate.estimated_retention_days ?? undefined
			})
		}),
		restartRequired: result.restart_required
	});
}
