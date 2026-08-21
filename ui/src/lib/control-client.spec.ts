import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import { durationFromMs, timestampFromDate } from '@bufbuild/protobuf/wkt';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	CameraManufacturerResultSchema,
	CameraDiscoveryResultSchema,
	CameraDeviceCapabilitiesSchema,
	CameraInfoSchema,
	CameraConfigurationResultSchema,
	CameraSettingsSchema,
	CameraHealthSnapshotSchema,
	CatalogHealthSnapshotSchema,
	CameraBackend as ProtoCameraBackend,
	CameraTransport as ProtoCameraTransport,
	CodecDescriptorSchema,
	ControlEnvelopeSchema,
	DiscoveredCameraSchema,
	DiskHealthSnapshotSchema,
	ExportDownloadResultSchema,
	ExportFileChunkSchema,
	ExportJobListSchema,
	ExportJobSchema,
	ExportJobStatus,
	ExportMessageSchema,
	HealthIssueSnapshotSchema,
	HealthProfileSummarySchema,
	HealthTotalsSnapshotSchema,
	LogBufferStatsSchema,
	LoadHealthSnapshotSchema,
	LoggingSettingsResultSchema,
	MemoryHealthSnapshotSchema,
	MotionDetectionResultSchema,
	MediaDataFormatSchema,
	MediaStreamCapabilitySchema,
	MediaVariantCapabilitySchema,
	MessageSchema,
	NotificationSchema,
	OkSchema,
	PtzPresetSchema,
	PtzResultSchema,
	RecordingCapacityEstimateSchema,
	RecordingDemandHealthSnapshotSchema,
	ProcessHealthSnapshotSchema,
	ResponseSchema,
	RuntimeConfigurationResultSchema,
	RuntimeStorageConfigurationSchema,
	SanitizedRuntimeConfigurationSchema,
	ServerCapabilitiesSchema,
	ServerHealthSnapshotSchema,
	SourceSessionSchema,
	StorageHealthSnapshotSchema,
	StreamHealthSnapshotSchema,
	SystemHealthSnapshotSchema,
	StoredMediaSourceCapabilitySchema,
	StoredMediaStreamCapabilitySchema,
	StoredMediaDeliverySchema,
	StoredMediaFragmentSchema,
	StoredMediaInitializationSchema,
	StoredMediaStateSchema,
	StoredMediaStatus,
	VideoDataFormatSchema,
	WebRtcHealthSnapshotSchema
} from './proto/webrtc_pb';

const api = vi.hoisted(() => ({
	createSession: vi.fn(),
	deleteSession: vi.fn()
}));

vi.mock('./api', () => api);

import { ControlClient, StoredMediaPlayback } from './control-client';

class FakeDataChannel {
	readyState: RTCDataChannelState = 'connecting';
	binaryType: BinaryType = 'blob';
	onopen: (() => void) | null = null;
	onclose: (() => void) | null = null;
	onerror: (() => void) | null = null;
	onmessage: ((event: MessageEvent) => void) | null = null;

	constructor(
		readonly label: string,
		readonly options: RTCDataChannelInit
	) {}
	activeFilter = 'info';
	ptzActions: string[] = [];

	send(data: ArrayBuffer | ArrayBufferView): void {
		if (this.label !== 'control-channel') return;
		const bytes =
			data instanceof ArrayBuffer
				? new Uint8Array(data)
				: new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
		const request = fromBinary(ControlEnvelopeSchema, bytes);
		if (request.message.case !== 'request') throw new Error('expected request');
		const command = request.message.value.command;
		const result =
			command.case === 'exportCommand'
				? (() => {
						const action = command.value.action;
						const exportJob = (status: ExportJobStatus) =>
							create(ExportJobSchema, {
								jobId: action.case === 'create' ? action.value.jobId : 'export-test',
								sourceId: 'front-door',
								streamId: 'main',
								requestedStartTime: timestampFromDate(new Date('2026-08-20T06:20:00Z')),
								requestedEndTime: timestampFromDate(new Date('2026-08-20T06:30:00Z')),
								alignedStartTime: timestampFromDate(new Date('2026-08-20T06:19:59Z')),
								status,
								progressPerMille: status === ExportJobStatus.READY ? 1_000 : 0,
								bytesWritten: status === ExportJobStatus.READY ? 3n : 0n,
								estimatedBytes: 3n,
								fileName: status === ExportJobStatus.READY ? 'front-door-export.mp4' : undefined,
								sha256:
									status === ExportJobStatus.READY
										? '039058c6f2c0cb492c533b0a4d14ef77cc0f78abccced5287d84a1a2011cfb81'
										: undefined,
								expiresAt:
									status === ExportJobStatus.READY
										? timestampFromDate(new Date('2026-08-21T06:30:00Z'))
										: undefined,
								retryable: status === ExportJobStatus.FAILED
							});
						if (action.case === 'list') {
							return {
								case: 'exportJobs' as const,
								value: create(ExportJobListSchema, {
									jobs: [exportJob(ExportJobStatus.RUNNING)]
								})
							};
						}
						if (action.case === 'download') {
							const data = create(MessageSchema, {
								message: {
									case: 'export',
									value: create(ExportMessageSchema, {
										message: {
											case: 'fileChunk',
											value: create(ExportFileChunkSchema, {
												jobId: action.value.jobId,
												chunkCount: 1,
												payload: new Uint8Array([1, 2, 3])
											})
										}
									})
								}
							});
							const encoded = toBinary(MessageSchema, data);
							const reliable = FakePeerConnection.latest?.channels.find(
								(channel) => channel.label === 'reliable-data'
							);
							queueMicrotask(() => reliable?.onmessage?.({ data: encoded.buffer } as MessageEvent));
							return {
								case: 'exportDownload' as const,
								value: create(ExportDownloadResultSchema, {
									job: exportJob(ExportJobStatus.READY),
									channel: 1,
									chunkCount: 1
								})
							};
						}
						return {
							case: 'exportJob' as const,
							value: exportJob(
								action.case === 'get'
									? ExportJobStatus.READY
									: action.case === 'cancel'
										? ExportJobStatus.CANCELLED
										: action.case === 'retry'
											? ExportJobStatus.RUNNING
											: ExportJobStatus.RUNNING
							)
						};
					})()
				: command.case === 'cameraControlCommand'
					? (() => {
							const action = command.value.action;
							return action.case === 'ptz'
								? (() => {
										const ptzAction = action.value.action.case;
										this.ptzActions.push(ptzAction ?? 'missing');
										return {
											case: 'ptzResult' as const,
											value: create(PtzResultSchema, {
												sourceId: action.value.sourceId,
												presets:
													ptzAction === 'listPresets'
														? [create(PtzPresetSchema, { presetId: 7, name: 'Gate' })]
														: []
											})
										};
									})()
								: action.case === 'getMotionDetection'
									? {
											case: 'motionDetectionResult' as const,
											value: create(MotionDetectionResultSchema, {
												supported: true,
												controllable: false,
												enabled: true
											})
										}
									: action.case === 'setMotionDetection'
										? {
												case: 'motionDetectionResult' as const,
												value: create(MotionDetectionResultSchema, {
													supported: true,
													controllable: true,
													enabled: action.value.enabled
												})
											}
										: action.case === 'setManufacturer'
											? {
													case: 'cameraManufacturerResult' as const,
													value: create(CameraManufacturerResultSchema, {
														sourceId: action.value.sourceId,
														manufacturer:
															action.value.manufacturer?.value.case === 'set'
																? action.value.manufacturer.value.value
																: 'ONVIF'
													})
												}
											: (() => {
													throw new Error('unexpected camera action');
												})();
						})()
					: command.case === 'loggingCommand'
						? (() => {
								if (command.value.action.case === 'setFilter') {
									this.activeFilter = command.value.action.value.filter;
								}
								return {
									case: 'loggingSettingsResult' as const,
									value: create(LoggingSettingsResultSchema, {
										activeFilter: this.activeFilter,
										defaultFilter: 'info,keeppeek=debug',
										version: '0.1.0',
										buffer: create(LogBufferStatsSchema, {
											entryCount: 4n,
											byteCount: 512n,
											maxEntries: 10_000n,
											maxBytes: 1_000_000n,
											maxStreams: 8n
										})
									})
								};
							})()
						: command.case === 'healthCommand' && command.value.action.case === 'get'
							? {
									case: 'healthResult' as const,
									value: create(ServerHealthSnapshotSchema, {
										status: 'degraded',
										generatedAtMs: 1_777_000_000_000n,
										uptimeSeconds: 3_600n,
										version: '0.1.0',
										totals: create(HealthTotalsSnapshotSchema, {
											configuredCameras: 1n,
											reportingCameras: 1n,
											ingressFps: 24.5,
											ingressBitrateBps: 1_500_000n
										}),
										system: create(SystemHealthSnapshotSchema, {
											hostName: 'keeppeek.local',
											architecture: 'arm64',
											logicalCores: 8n,
											systemCpuPercent: 12.5,
											process: create(ProcessHealthSnapshotSchema, {
												pid: 42,
												residentMemoryBytes: 536_870_912n
											}),
											memory: create(MemoryHealthSnapshotSchema, {
												totalBytes: 16_000_000_000n,
												usedBytes: 8_000_000_000n,
												availableBytes: 8_000_000_000n
											}),
											load: create(LoadHealthSnapshotSchema, { oneMinute: 1.5 }),
											disks: [
												create(DiskHealthSnapshotSchema, {
													name: 'recordings',
													mountPoint: '/recordings',
													totalBytes: 2_000_000_000n,
													availableBytes: 500_000_000n,
													usedBytes: 1_500_000_000n,
													storesRecordings: true
												})
											]
										}),
										storage: create(StorageHealthSnapshotSchema, {
											mediumTermPath: '/recordings',
											longTermPath: '/recordings',
											pathsAreSame: true,
											catalog: create(CatalogHealthSnapshotSchema, { events: 12n }),
											demand: create(RecordingDemandHealthSnapshotSchema, {
												activeStreams: 1n,
												totalViewers: 2n
											})
										}),
										webrtc: create(WebRtcHealthSnapshotSchema, {
											activeSessions: 1n,
											multiTrackSessions: 1n,
											multiTracks: 2n
										}),
										cameras: [
											create(CameraHealthSnapshotSchema, {
												id: 'front-door',
												ip: '192.0.2.10',
												name: 'Front Door',
												backend: 'Reolink',
												transport: 'TCP',
												state: 'degraded',
												configuredProfiles: [
													create(HealthProfileSummarySchema, {
														name: 'Main',
														stream: 'main',
														encoding: 'h264'
													})
												],
												streams: [
													create(StreamHealthSnapshotSchema, {
														type: 'video_main',
														fps: 24.5,
														drops: 3n,
														updatedAtMs: 1_777_000_000_000n,
														reportAgeMs: 200n
													})
												]
											})
										],
										issues: [
											create(HealthIssueSnapshotSchema, {
												severity: 'warning',
												scope: 'Front Door',
												message: 'Measured stream FPS is low'
											})
										]
									})
								}
							: command.case === 'serverCommand' && command.value.action.case === 'restart'
								? { case: 'restartResult' as const, value: { restarting: true } }
								: command.case === 'cameraConfigurationCommand' &&
									  command.value.action.case === 'get'
									? {
											case: 'cameraConfigurationResult' as const,
											value: create(CameraConfigurationResultSchema, {
												cameras: [
													create(CameraSettingsSchema, {
														id: '192.0.2.50',
														ip: '192.0.2.50',
														displayName: 'Gate',
														usernameConfigured: true,
														passwordConfigured: true,
														backend: ProtoCameraBackend.REO_PROTO,
														transport: ProtoCameraTransport.TCP,
														health: 'online',
														model: 'RLC-811A'
													})
												]
											})
										}
									: command.case === 'cameraConfigurationCommand' &&
										  command.value.action.case === 'discover'
										? {
												case: 'cameraDiscoveryResult' as const,
												value: create(CameraDiscoveryResultSchema, {
													cameras: [
														create(DiscoveredCameraSchema, {
															ip: '192.0.2.50',
															brand: 'reolink',
															name: 'Gate',
															onvifPort: 8000,
															sources: ['onvif'],
															configured: false
														})
													]
												})
											}
										: command.case === 'cameraConfigurationCommand' &&
											  command.value.action.case === 'update'
											? (() => {
													const update = command.value.action.value;
													if (update.username !== undefined || update.password !== undefined) {
														throw new Error('omitted credentials must remain absent');
													}
													if (update.mainRtspUrl?.value.case !== 'clear') {
														throw new Error('null RTSP URL must encode as clear');
													}
													return {
														case: 'cameraConfigurationResult' as const,
														value: create(CameraConfigurationResultSchema, {
															camera: create(CameraSettingsSchema, {
																id: update.ip,
																ip: update.ip,
																displayName:
																	update.displayName?.value.case === 'set'
																		? update.displayName.value.value
																		: undefined,
																usernameConfigured: true,
																passwordConfigured: true,
																backend: update.backend,
																transport: update.transport
															}),
															restartRequired: true
														})
													};
												})()
											: command.case === 'cameraConfigurationCommand' &&
												  command.value.action.case === 'remove'
												? {
														case: 'cameraConfigurationResult' as const,
														value: create(CameraConfigurationResultSchema, { removed: true })
													}
												: command.case === 'runtimeConfigurationCommand' &&
													  command.value.action.case === 'get'
													? {
															case: 'runtimeConfigurationResult' as const,
															value: create(RuntimeConfigurationResultSchema, {
																config: create(SanitizedRuntimeConfigurationSchema, {
																	host: '0.0.0.0',
																	port: 3000,
																	storage: create(RuntimeStorageConfigurationSchema, {
																		mediumTermPath: '/recordings/medium',
																		longTermPath: '/recordings/long',
																		recordingCatalogPath: '/metadata/recordings.db',
																		eventThumbnailPath: '/metadata/thumbnails'
																	}),
																	cameraCount: 1n,
																	recordingEstimate: create(RecordingCapacityEstimateSchema)
																})
															})
														}
													: command.case === 'runtimeConfigurationCommand' &&
														  command.value.action.case === 'update'
														? {
																case: 'runtimeConfigurationResult' as const,
																value: create(RuntimeConfigurationResultSchema, {
																	config: create(SanitizedRuntimeConfigurationSchema, {
																		host: command.value.action.value.host,
																		port: command.value.action.value.port,
																		storage: command.value.action.value.storage,
																		cameraCount: 2n,
																		recordingEstimate: create(RecordingCapacityEstimateSchema, {
																			estimatedBitrateBps: 8_000_000n,
																			bytesPerDay: 86_400_000_000n,
																			knownStreams: 2n,
																			estimatedRetentionDays: 2.5
																		})
																	}),
																	restartRequired: true
																})
															}
														: (() => {
																throw new Error('unexpected control command');
															})();
		const response = create(ControlEnvelopeSchema, {
			message: {
				case: 'response',
				value: create(ResponseSchema, {
					requestId: request.message.value.requestId,
					result: {
						case: 'ok',
						value: create(OkSchema, {
							result
						})
					}
				})
			}
		});
		const encoded = toBinary(ControlEnvelopeSchema, response);
		queueMicrotask(() => this.onmessage?.({ data: encoded.buffer } as MessageEvent));
	}

	open(): void {
		this.readyState = 'open';
		this.onopen?.();
		if (this.label === 'control-channel') {
			const notification = create(ControlEnvelopeSchema, {
				message: {
					case: 'notification',
					value: create(NotificationSchema, {
						event: {
							case: 'initialCapabilities',
							value: create(ServerCapabilitiesSchema, {
								revision: 1n,
								capabilityIds: ['keeppeek.runtime-config.v1'],
								cameras: [
									create(CameraInfoSchema, {
										sourceId: 'front-door',
										displayName: 'Front Door',
										manufacturer: 'Reolink',
										model: 'RLC-811A',
										ip: '192.0.2.10',
										webUrl: 'http://192.0.2.10',
										httpPort: 80,
										rtspPort: 554,
										onvifPort: 8000,
										isReolink: true,
										deviceCapabilities: create(CameraDeviceCapabilitiesSchema, {
											audio: true,
											events: true,
											recording: true
										})
									}),
									create(CameraInfoSchema, {
										sourceId: 'garage',
										displayName: 'Garage',
										ip: '192.0.2.11'
									})
								],
								sourceSessions: [
									create(SourceSessionSchema, {
										sourceSessionId: 'camera:front-door',
										sourceId: 'front-door',
										displayName: 'Front Door',
										video: create(MediaStreamCapabilitySchema, {
											variants: [
												create(MediaVariantCapabilitySchema, {
													variantId: 'sub',
													codec: create(CodecDescriptorSchema, { name: 'H264' }),
													format: create(MediaDataFormatSchema, {
														format: {
															case: 'video',
															value: create(VideoDataFormatSchema, {
																width: 640,
																height: 360
															})
														}
													}),
													nominalBitrateBps: 1_500_000n
												})
											]
										})
									})
								],
								storedMediaSources: [
									create(StoredMediaSourceCapabilitySchema, {
										sourceId: 'garage',
										displayName: 'Garage',
										streams: [
											create(StoredMediaStreamCapabilitySchema, {
												streamId: 'main',
												contentType: 'video/mp4'
											})
										]
									})
								]
							})
						}
					})
				}
			});
			const encoded = toBinary(ControlEnvelopeSchema, notification);
			queueMicrotask(() => this.onmessage?.({ data: encoded.buffer } as MessageEvent));
		}
	}

	close(): void {
		this.readyState = 'closed';
		this.onclose?.();
	}
}

class FakePeerConnection {
	static latest: FakePeerConnection | null = null;
	readonly channels: FakeDataChannel[] = [];
	localDescription: RTCSessionDescription | null = null;
	connectionState: RTCPeerConnectionState = 'new';
	onconnectionstatechange: (() => void) | null = null;

	constructor() {
		FakePeerConnection.latest = this;
	}

	createDataChannel(label: string, options: RTCDataChannelInit): RTCDataChannel {
		const channel = new FakeDataChannel(label, options);
		this.channels.push(channel);
		return channel as unknown as RTCDataChannel;
	}

	async createOffer(): Promise<RTCSessionDescriptionInit> {
		return { type: 'offer', sdp: 'v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel' };
	}

	async setLocalDescription(description: RTCSessionDescriptionInit): Promise<void> {
		this.localDescription = description as RTCSessionDescription;
	}

	async setRemoteDescription(): Promise<void> {
		for (const channel of this.channels) channel.open();
	}

	close(): void {
		this.connectionState = 'closed';
	}
}

afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
	vi.clearAllMocks();
	FakePeerConnection.latest = null;
});

describe('ControlClient', () => {
	it('uses the canonical negotiated channels and correlates binary motion control', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'session-42',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const client = new ControlClient();
		let advertised: readonly string[] = [];
		client.onCapabilities((capabilityIds) => {
			advertised = capabilityIds;
		});

		await expect(client.setMotionDetection('front-door', false)).resolves.toEqual({
			supported: true,
			controllable: true,
			enabled: false,
			error: null
		});
		await expect(client.getCameras()).resolves.toEqual([
			{
				id: 'front-door',
				ip: '192.0.2.10',
				name: 'Front Door',
				manufacturer: 'Reolink',
				model: 'RLC-811A',
				firmware_version: null,
				serial_number: null,
				hardware_id: null,
				hostname: null,
				mac_address: null,
				is_reolink: true,
				web_url: 'http://192.0.2.10',
				ports: { http: 80, https: null, rtsp: 554, onvif: 8000 },
				capabilities: {
					ptz: false,
					audio: true,
					events: true,
					recording: true,
					analytics: false,
					imaging: false,
					two_way_audio: false
				},
				profiles: [
					{
						name: 'subStream',
						stream: 'sub',
						encoding: 'h264',
						resolution: '640x360',
						framerate: null,
						bitrate_kbps: 1500,
						gop: null,
						h264_profile: null,
						audio: null
					}
				]
			},
			{
				id: 'garage',
				ip: '192.0.2.11',
				name: 'Garage',
				manufacturer: null,
				model: null,
				firmware_version: null,
				serial_number: null,
				hardware_id: null,
				hostname: null,
				mac_address: null,
				is_reolink: false,
				web_url: undefined,
				ports: { http: null, https: null, rtsp: null, onvif: null },
				capabilities: {
					ptz: false,
					audio: false,
					events: false,
					recording: false,
					analytics: false,
					imaging: false,
					two_way_audio: false
				},
				profiles: [
					{
						name: 'mainStream',
						stream: 'main',
						encoding: null,
						resolution: null,
						framerate: null,
						bitrate_kbps: null,
						gop: null,
						h264_profile: null,
						audio: null
					}
				]
			}
		]);
		await expect(client.getHealth()).resolves.toMatchObject({
			status: 'degraded',
			generated_at_ms: 1_777_000_000_000,
			totals: { configured_cameras: 1, ingress_fps: 24.5 },
			system: {
				host_name: 'keeppeek.local',
				process: { pid: 42, resident_memory_bytes: 536_870_912 },
				disks: [{ mount_point: '/recordings', stores_recordings: true }]
			},
			storage: { catalog: { events: 12 }, demand: { active_streams: 1, total_viewers: 2 } },
			webrtc: { active_sessions: 1, browser_sessions: 1, browser_tracks: 2 },
			cameras: [
				{
					id: 'front-door',
					state: 'degraded',
					configured_profiles: [{ stream: 'main', encoding: 'h264' }],
					streams: [{ type: 'video_main', fps: 24.5, drops: 3 }]
				}
			],
			issues: [{ severity: 'warning', scope: 'Front Door' }]
		});
		await expect(client.getCameraDetails('front-door')).resolves.toMatchObject({
			camera: {
				id: 'front-door',
				backend: 'Reolink',
				transport: 'TCP',
				ports: { http: 80, rtsp: 554, onvif: 8000 },
				profiles: [{ stream: 'main', encoding: 'h264' }]
			},
			health: { id: 'front-door', state: 'degraded' },
			motion_detection: { supported: true, controllable: false, enabled: true, error: null }
		});
		await expect(
			client.movePtz('front-door', { pan: 1, tilt: 0, zoom: 0 })
		).resolves.toBeUndefined();
		await expect(client.stopPtz('front-door')).resolves.toBeUndefined();
		await expect(client.getPtzPresets('front-door')).resolves.toEqual([{ id: 7, name: 'Gate' }]);
		await expect(client.gotoPtzPreset('front-door', 7)).resolves.toBeUndefined();
		const createdExport = await client.createExport({
			sourceId: 'front-door',
			streamId: 'main',
			startMs: Date.parse('2026-08-20T06:20:00Z'),
			endMs: Date.parse('2026-08-20T06:30:00Z')
		});
		expect(createdExport).toMatchObject({
			id: 'export-1',
			status: 'running',
			sourceId: 'front-door',
			estimatedBytes: 3
		});
		await expect(client.listExports()).resolves.toMatchObject([
			{ status: 'running', sourceId: 'front-door' }
		]);
		await expect(client.getExport('export-test')).resolves.toMatchObject({
			status: 'ready',
			fileName: 'front-door-export.mp4',
			bytesWritten: 3,
			sha256: '039058c6f2c0cb492c533b0a4d14ef77cc0f78abccced5287d84a1a2011cfb81'
		});
		await expect(client.cancelExport('export-test')).resolves.toMatchObject({
			status: 'cancelled'
		});
		await expect(client.retryExport('export-test')).resolves.toMatchObject({ status: 'running' });
		const downloaded = await client.downloadExport('export-test');
		expect(downloaded.job.status).toBe('ready');
		expect(new Uint8Array(await downloaded.blob.arrayBuffer())).toEqual(new Uint8Array([1, 2, 3]));
		await expect(client.setCameraManufacturer('front-door', 'Hikvision')).resolves.toBe(
			'Hikvision'
		);
		await expect(client.setCameraManufacturer('front-door', null)).resolves.toBe('ONVIF');
		await expect(client.getLoggingSettings()).resolves.toMatchObject({ active_filter: 'info' });
		await expect(client.setLoggingFilter('warn,str0m=error')).resolves.toMatchObject({
			active_filter: 'warn,str0m=error',
			buffer: { entry_count: 4, byte_count: 512 }
		});
		await expect(client.restartServer()).resolves.toBeUndefined();
		await expect(client.discoverCameras([137])).resolves.toEqual([
			{
				ip: '192.0.2.50',
				brand: 'reolink',
				name: 'Gate',
				model: null,
				onvif_port: 8000,
				sources: ['onvif'],
				configured: false,
				health: null
			}
		]);
		await expect(client.getCameraSettings()).resolves.toEqual([
			{
				id: '192.0.2.50',
				ip: '192.0.2.50',
				display_name: 'Gate',
				manufacturer_override: null,
				username_configured: true,
				password_configured: true,
				onvif_port: null,
				http_port: null,
				main_rtsp_url: null,
				sub_rtsp_url: null,
				uid_configured: false,
				backend: 'reo-proto',
				transport: 'tcp',
				health: 'online',
				model: 'RLC-811A'
			}
		]);
		await expect(client.getRuntimeConfiguration()).resolves.toMatchObject({
			host: '0.0.0.0',
			port: 3000,
			camera_count: 1,
			storage: { recording_catalog_path: '/metadata/recordings.db' }
		});
		await expect(
			client.updateCamera('192.0.2.50', {
				display_name: 'Updated Gate',
				main_rtsp_url: null,
				backend: 'reo-proto',
				transport: 'udp'
			})
		).resolves.toMatchObject({
			camera: {
				ip: '192.0.2.50',
				display_name: 'Updated Gate',
				password_configured: true,
				main_rtsp_url: null,
				backend: 'reo-proto',
				transport: 'udp'
			},
			restart_required: true
		});
		await expect(client.removeCamera('192.0.2.50')).resolves.toBeUndefined();
		await expect(
			client.updateRuntimeConfiguration({
				host: '127.0.0.1',
				port: 3200,
				move_existing_recordings: true,
				storage: {
					medium_term_path: '/recordings/medium',
					long_term_path: '/recordings/long',
					recording_catalog_path: '/metadata/recordings.db',
					event_thumbnail_path: '/metadata/thumbnails',
					event_thumbnail_max_mb: 512,
					short_term_secs: 30,
					medium_term_secs: 120,
					flush_interval_secs: 15,
					write_buffer_bytes: 16_384,
					long_term_max_gb: 24
				}
			})
		).resolves.toMatchObject({
			config: {
				host: '127.0.0.1',
				port: 3200,
				camera_count: 2,
				storage: {
					recording_catalog_path: '/metadata/recordings.db',
					write_buffer_bytes: 16_384
				},
				recording_estimate: { estimated_retention_days: 2.5 }
			},
			restart_required: true
		});
		expect(advertised).toEqual(['keeppeek.runtime-config.v1']);
		const peer = FakePeerConnection.latest;
		expect(peer?.channels.map((channel) => [channel.label, channel.options])).toEqual([
			['control-channel', { negotiated: true, id: 0, ordered: true }],
			['reliable-data', { negotiated: true, id: 1, ordered: true }],
			['unreliable-data', { negotiated: true, id: 2, ordered: false, maxRetransmits: 0 }]
		]);
		expect(peer?.channels[0]?.ptzActions).toEqual([
			'continuous',
			'stop',
			'listPresets',
			'gotoPreset'
		]);

		await client.close();
		expect(api.deleteSession).toHaveBeenCalledWith('session-42');
	});

	it('refills at half-buffer and ends only after the terminal generation is appended', async () => {
		class FakeSourceBuffer extends EventTarget {
			updating = false;
			mode: AppendMode = 'segments';
			appendCount = 0;

			appendBuffer(): void {
				this.appendCount += 1;
				this.dispatchEvent(new Event('updateend'));
			}
		}

		class FakeMediaSource extends EventTarget {
			static latest: FakeMediaSource | null = null;
			static isTypeSupported(): boolean {
				return true;
			}

			readyState: ReadyState = 'open';
			duration = Number.NaN;
			readonly sourceBuffer = new FakeSourceBuffer();
			endCount = 0;

			constructor() {
				super();
				FakeMediaSource.latest = this;
			}

			addSourceBuffer(): SourceBuffer {
				return this.sourceBuffer as unknown as SourceBuffer;
			}

			endOfStream(): void {
				this.endCount += 1;
				this.readyState = 'ended';
			}
		}

		vi.stubGlobal('MediaSource', FakeMediaSource);
		vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:stored-test');
		vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => undefined);

		const anchorMs = Date.UTC(2026, 7, 20, 12);
		const state = (generation: bigint, status: StoredMediaStatus) =>
			create(StoredMediaStateSchema, {
				storedMediaId: 'review-test',
				status,
				generation,
				requestedTime: timestampFromDate(new Date(anchorMs)),
				fragmentTime: timestampFromDate(new Date(anchorMs)),
				endTime: timestampFromDate(new Date(anchorMs + 2_000)),
				mode: 1,
				playing: true,
				playbackRate: 1,
				delivery: create(StoredMediaDeliverySchema, {
					mediaChannel: 1,
					contentType: 'video/mp4; codecs="avc1.42E01E"',
					maxBufferDuration: durationFromMs(1_000)
				})
			});
		const refill = vi.fn(async () => state(2n, StoredMediaStatus.ENDED));
		const playback = new StoredMediaPlayback(
			'review-test',
			refill,
			vi.fn(async () => state(1n, StoredMediaStatus.ACTIVE)),
			vi.fn(async () => {})
		);
		playback.configure(state(1n, StoredMediaStatus.ACTIVE));
		playback.receiveInitialization(
			create(StoredMediaInitializationSchema, {
				storedMediaId: 'review-test',
				generation: 1n,
				initializationId: 1n,
				contentType: 'video/mp4; codecs="avc1.42E01E"',
				chunkCount: 1,
				payload: new Uint8Array([1])
			})
		);
		playback.receiveFragment(
			create(StoredMediaFragmentSchema, {
				storedMediaId: 'review-test',
				generation: 1n,
				initializationId: 1n,
				sequence: 1n,
				startTime: timestampFromDate(new Date(anchorMs)),
				duration: durationFromMs(1_000),
				chunkCount: 2,
				payload: new Uint8Array([2])
			})
		);

		playback.observe(0.6);
		expect(refill).not.toHaveBeenCalled();
		playback.receiveFragment(
			create(StoredMediaFragmentSchema, {
				storedMediaId: 'review-test',
				generation: 1n,
				initializationId: 1n,
				sequence: 1n,
				startTime: timestampFromDate(new Date(anchorMs)),
				duration: durationFromMs(1_000),
				chunkIndex: 1,
				chunkCount: 2,
				payload: new Uint8Array([5])
			})
		);
		playback.observe(0.6);
		await vi.waitFor(() => expect(refill).toHaveBeenCalledWith(anchorMs + 600));
		expect(FakeMediaSource.latest?.endCount).toBe(0);

		playback.receiveInitialization(
			create(StoredMediaInitializationSchema, {
				storedMediaId: 'review-test',
				generation: 2n,
				initializationId: 1n,
				contentType: 'video/mp4; codecs="avc1.42E01E"',
				chunkCount: 1,
				payload: new Uint8Array([3])
			})
		);
		playback.receiveFragment(
			create(StoredMediaFragmentSchema, {
				storedMediaId: 'review-test',
				generation: 2n,
				initializationId: 1n,
				sequence: 1n,
				startTime: timestampFromDate(new Date(anchorMs + 1_000)),
				duration: durationFromMs(1_000),
				chunkCount: 1,
				payload: new Uint8Array([4])
			})
		);

		await vi.waitFor(() => expect(FakeMediaSource.latest?.endCount).toBe(1));
		expect(FakeMediaSource.latest?.sourceBuffer.appendCount).toBe(4);
	});
});
