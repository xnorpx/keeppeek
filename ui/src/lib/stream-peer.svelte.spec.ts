import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	CameraInfoSchema,
	DeliveryTransport,
	MediaKind,
	type SubscribeMedia,
	ConnectionUpdateSchema,
	ControlEnvelopeSchema,
	LiveQualityDiagnosticsSchema,
	LiveQualitySelectionReason,
	MediaStreamCapabilitySchema,
	MediaVariantCapabilitySchema,
	NotificationSchema,
	OkSchema,
	ResponseSchema,
	RtpDeliverySchema,
	ServerCapabilitiesSchema,
	SourceSessionSchema,
	SourceSessionRemovedSchema,
	SubscriptionResultSchema,
	SubscriptionStreamStateSchema,
	type LiveQualityDiagnostics,
	type Notification
} from './proto/webrtc_pb';

const api = vi.hoisted(() => ({
	createSession: vi.fn(),
	deleteSession: vi.fn()
}));

vi.mock('./api', async (load) => ({ ...(await load<typeof import('./api')>()), ...api }));
import { ApiRequestError } from './api';

import { LivePeer } from './stream-peer.svelte';
import { AacAudioPlayback } from './audio-playback';

class FakeDataChannel {
	static suppressCapabilities = false;
	static audioVariants: string[] = [];
	static audioUsesReliable = false;
	static audioBindingSuffix = '';
	static offlineCamera: string | null = null;
	static rejectedSubscription: string | null = null;
	static rejectUnsubscribe = false;
	holdUnsubscribe = false;
	releaseUnsubscribe: (() => void) | undefined;
	static holdNextSavedAudio = false;
	releaseSavedAudio: (() => void) | undefined;
	holdSavedVideo = false;
	releaseSavedVideo: (() => void) | undefined;
	holdCameraCommand = false;
	static subscriptionQuality: LiveQualityDiagnostics | undefined;
	static selectedVariant: string | null = null;

	readyState: RTCDataChannelState = 'connecting';
	binaryType: BinaryType = 'blob';
	onopen: (() => void) | null = null;
	onclose: (() => void) | null = null;
	onerror: (() => void) | null = null;
	onmessage: ((event: MessageEvent) => void) | null = null;
	readonly commands: string[] = [];
	readonly subscriptions: SubscribeMedia[] = [];
	readonly unsubscribed: string[][] = [];

	constructor(
		readonly label: string,
		readonly options: RTCDataChannelInit
	) {}

	send(data: ArrayBuffer | ArrayBufferView): void {
		if (this.label !== 'control-channel') return;
		const bytes =
			data instanceof ArrayBuffer
				? new Uint8Array(data)
				: new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
		const envelope = fromBinary(ControlEnvelopeSchema, bytes);
		if (envelope.message.case !== 'request') throw new Error('expected control request');
		const request = envelope.message.value;
		const command = request.command;
		this.commands.push(command.case ?? 'unknown');
		if (command.case === 'subscribeMedia') this.subscriptions.push(command.value);
		if (command.case === 'unsubscribe') this.unsubscribed.push(command.value.subscriptionIds);

		const result =
			command.case === 'subscribeMedia'
				? {
						case: 'subscriptionResult' as const,
						value: create(SubscriptionResultSchema, {
							subscriptionId: command.value.subscriptionId,
							delivery:
								command.value.kind === MediaKind.AUDIO && FakeDataChannel.audioUsesReliable
									? {
											case: 'mediaData',
											value: {
												streamBindingId:
													command.value.subscriptionId + FakeDataChannel.audioBindingSuffix
											}
										}
									: {
											case: 'rtp',
											value: create(RtpDeliverySchema, {
												mid: command.value.subscriptionId.replace('camera-', '')
											})
										},
							selectedVariantId:
								FakeDataChannel.selectedVariant ?? (command.value.variantId || 'sub'),
							liveAudio: {
								preview: command.value.liveAudioPreviewOffsetMs !== undefined,
								appliedOffsetMs: command.value.liveAudioPreviewOffsetMs ?? 0
							},
							liveQuality:
								FakeDataChannel.subscriptionQuality ??
								(command.value.kind === MediaKind.VIDEO &&
								command.value.liveAudioPreviewOffsetMs !== undefined
									? create(LiveQualityDiagnosticsSchema, {
											activeVariantId: command.value.variantId
										})
									: undefined)
						})
					}
				: undefined;
		const response = create(ControlEnvelopeSchema, {
			message: {
				case: 'response',
				value: create(ResponseSchema, {
					requestId: request.requestId,
					result:
						(command.case === 'subscribeMedia' &&
							command.value.subscriptionId === FakeDataChannel.rejectedSubscription) ||
						(command.case === 'unsubscribe' && FakeDataChannel.rejectUnsubscribe)
							? { case: 'error', value: { message: 'Stream capacity reached' } }
							: {
									case: 'ok',
									value: create(OkSchema, { result })
								}
				})
			}
		});
		if (this.holdCameraCommand && command.case === 'cameraControlCommand') return;
		const encoded = toBinary(ControlEnvelopeSchema, response);
		const deliver = () =>
			queueMicrotask(() => this.onmessage?.({ data: encoded.buffer } as MessageEvent));
		if (command.case === 'unsubscribe' && this.holdUnsubscribe) {
			this.holdUnsubscribe = false;
			this.releaseUnsubscribe = deliver;
		} else if (
			this.holdSavedVideo &&
			command.case === 'subscribeMedia' &&
			command.value.kind === MediaKind.VIDEO &&
			command.value.liveAudioPreviewOffsetMs === undefined
		) {
			this.holdSavedVideo = false;
			this.releaseSavedVideo = deliver;
		} else if (
			FakeDataChannel.holdNextSavedAudio &&
			command.case === 'subscribeMedia' &&
			command.value.kind === MediaKind.AUDIO &&
			command.value.liveAudioPreviewOffsetMs === undefined
		) {
			FakeDataChannel.holdNextSavedAudio = false;
			this.releaseSavedAudio = deliver;
		} else {
			deliver();
		}
	}

	open(): void {
		this.readyState = 'open';
		this.onopen?.();
		this.publishCapabilities();
	}

	publishCapabilities(): void {
		if (this.label !== 'control-channel' || FakeDataChannel.suppressCapabilities) return;
		const capabilities = create(ControlEnvelopeSchema, {
			message: {
				case: 'notification',
				value: create(NotificationSchema, {
					event: {
						case: 'initialCapabilities',
						value: create(ServerCapabilitiesSchema, {
							revision: 1n,
							cameras: FakeDataChannel.offlineCamera
								? [create(CameraInfoSchema, { sourceId: FakeDataChannel.offlineCamera })]
								: [],
							sourceSessions: ['front-door', 'garage'].map((cameraId) =>
								create(SourceSessionSchema, {
									sourceSessionId: `camera:${cameraId}`,
									sourceId: cameraId,
									displayName: cameraId,
									video: create(MediaStreamCapabilitySchema),
									audio: FakeDataChannel.audioVariants.length
										? create(MediaStreamCapabilitySchema, {
												variants: FakeDataChannel.audioVariants.map((variantId) =>
													create(MediaVariantCapabilitySchema, {
														variantId,
														codec: { name: 'aac' },
														format: {
															format: {
																case: 'audio',
																value: { sampleRateHz: 48000, channelCount: 1 }
															}
														},
														deliveryTransports: [
															FakeDataChannel.audioUsesReliable
																? DeliveryTransport.RELIABLE_DATA
																: DeliveryTransport.RTP
														]
													})
												)
											})
										: undefined
								})
							)
						})
					}
				})
			}
		});
		const encoded = toBinary(ControlEnvelopeSchema, capabilities);
		queueMicrotask(() => this.onmessage?.({ data: encoded.buffer } as MessageEvent));
	}

	close(): void {
		this.readyState = 'closed';
		this.onclose?.();
	}
}

class FakePeerConnection {
	static latest: FakePeerConnection | null = null;
	static failRemoteDescription = false;
	static holdNextControlOpen = false;

	readonly channels: FakeDataChannel[] = [];
	readonly transceivers: RTCRtpTransceiver[] = [];
	localDescription: RTCSessionDescription | null = null;
	connectionState: RTCPeerConnectionState = 'new';
	iceConnectionState: RTCIceConnectionState = 'new';
	iceGatheringState: RTCIceGatheringState = 'complete';
	ontrack: ((event: RTCTrackEvent) => void) | null = null;
	onconnectionstatechange: (() => void) | null = null;
	oniceconnectionstatechange: (() => void) | null = null;

	constructor() {
		FakePeerConnection.latest = this;
	}

	createDataChannel(label: string, options: RTCDataChannelInit): RTCDataChannel {
		const channel = new FakeDataChannel(label, options);
		this.channels.push(channel);
		return channel as unknown as RTCDataChannel;
	}

	addTransceiver(): RTCRtpTransceiver {
		const transceiver = {
			mid: String(this.transceivers.length),
			setCodecPreferences: vi.fn()
		} as unknown as RTCRtpTransceiver;
		this.transceivers.push(transceiver);
		return transceiver;
	}

	async createOffer(): Promise<RTCSessionDescriptionInit> {
		return { type: 'offer', sdp: 'v=0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel' };
	}

	async setLocalDescription(description: RTCSessionDescriptionInit): Promise<void> {
		this.localDescription = description as RTCSessionDescription;
	}

	async setRemoteDescription(): Promise<void> {
		const holdControl = FakePeerConnection.holdNextControlOpen;
		FakePeerConnection.holdNextControlOpen = false;
		for (const channel of this.channels) {
			if (!holdControl || channel.label !== 'control-channel') channel.open();
		}
		if (FakePeerConnection.failRemoteDescription) {
			throw new Error('remote description rejected');
		}
	}

	addEventListener(): void {}
	removeEventListener(): void {}

	close(): void {
		this.connectionState = 'closed';
	}
}

afterEach(() => {
	vi.unstubAllGlobals();
	vi.restoreAllMocks();
	vi.clearAllMocks();
	FakeDataChannel.suppressCapabilities = false;
	FakeDataChannel.audioVariants = [];
	FakeDataChannel.audioUsesReliable = false;
	FakeDataChannel.audioBindingSuffix = '';
	FakeDataChannel.offlineCamera = null;
	FakeDataChannel.rejectedSubscription = null;
	FakeDataChannel.holdNextSavedAudio = false;
	FakeDataChannel.rejectUnsubscribe = false;
	FakeDataChannel.subscriptionQuality = undefined;
	FakeDataChannel.selectedVariant = null;
	FakePeerConnection.failRemoteDescription = false;
	FakePeerConnection.holdNextControlOpen = false;
	FakePeerConnection.latest = null;
});

function notification(event: Notification['event']): MessageEvent {
	const envelope = create(ControlEnvelopeSchema, {
		message: {
			case: 'notification',
			value: create(NotificationSchema, { event })
		}
	});
	return new MessageEvent('message', { data: toBinary(ControlEnvelopeSchema, envelope).buffer });
}

function networkEstimate(bitsPerSecond: bigint): MessageEvent {
	return notification({
		case: 'connectionUpdate',
		value: create(ConnectionUpdateSchema, { availableBitrateBps: bitsPerSecond })
	});
}

function streamState(
	subscriptionId: string,
	activeVariantId: string,
	liveQuality?: LiveQualityDiagnostics
): MessageEvent {
	return notification({
		case: 'subscriptionStreamState',
		value: create(SubscriptionStreamStateSchema, { subscriptionId, activeVariantId, liveQuality })
	});
}

describe('LivePeer', () => {
	it('applies and resets the same viewer preview on both admitted tracks of the selected profile', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'timing',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['main', 'sub'];
		const peer = new LivePeer();
		for (const offset of [undefined, -80, 200, 0, undefined]) {
			await peer.configure([
				{
					cameraId: 'front-door',
					variantId: 'sub',
					quality: 'auto',
					liveAudioPreviewOffsetMs: offset
				}
			]);
			const channel = FakePeerConnection.latest!.channels[0];
			expect(
				channel.subscriptions
					.slice(-2)
					.map((s) => [s.kind, s.variantId, s.liveAudioPreviewOffsetMs])
			).toEqual([
				[MediaKind.VIDEO, 'sub', offset],
				[MediaKind.AUDIO, 'sub', offset]
			]);
			expect(peer.track('front-door')?.liveAudio?.preview).toBe(offset !== undefined);
		}
		expect(api.createSession).toHaveBeenCalledOnce();
		await peer.close();
	});

	it('rejects a preview when the chosen video variant has no matching audio', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'timing',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['main'];
		const peer = new LivePeer();
		await expect(
			peer.configure([
				{ cameraId: 'front-door', variantId: 'sub', quality: 'auto', liveAudioPreviewOffsetMs: 25 }
			])
		).rejects.toThrow('no matching live audio profile');
		expect(api.deleteSession).toHaveBeenCalledOnce();
	});

	it('rolls back the confirmed pair when switching profiles at the same preview offset', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'timing',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['sub'];
		const peer = new LivePeer();
		const sub = {
			cameraId: 'front-door',
			quality: 'auto',
			variantId: 'sub',
			liveAudioPreviewOffsetMs: 45
		} as const;
		await peer.configure([sub]);
		await expect(peer.configure([{ ...sub, variantId: 'main' }])).rejects.toThrow(
			'no matching live audio profile'
		);
		expect(peer.track(sub.cameraId)).toMatchObject({
			requestedVariantId: 'sub',
			audioVariantId: 'sub',
			liveAudioPreviewOffsetMs: 45
		});
		await peer.close();
	});

	it('applies a queued deactivation after an older preview rollback completes', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'timing',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['sub'];
		const peer = new LivePeer();
		const saved = { cameraId: 'front-door', quality: 'auto' as const };
		await peer.configure([saved]);
		const channel = FakePeerConnection.latest!.channels[0];
		channel.holdSavedVideo = true;
		const rejected = expect(
			peer.configure([{ ...saved, variantId: 'main', liveAudioPreviewOffsetMs: -80 }])
		).rejects.toThrow('matching live audio');
		await vi.waitFor(() => expect(channel.releaseSavedVideo).toBeDefined());
		const deactivate = peer.configure([{ ...saved, active: false }]);
		channel.releaseSavedVideo!();
		await rejected;
		await deactivate;
		expect(peer.track('front-door')?.subscribed).toBe(false);
		expect(channel.unsubscribed.flat()).toContain('camera-0');
		await peer.close();
	});

	it.each([false, true])(
		'restores saved timing with fallback or removed audio, video only %s',
		async (videoOnly) => {
			vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
			api.createSession.mockResolvedValue({
				session_id: 'timing',
				answer: { type: 'answer', sdp: 'v=0' }
			});
			api.deleteSession.mockResolvedValue(undefined);
			FakeDataChannel.audioVariants = ['sub'];
			const peer = new LivePeer();
			await peer.configure([
				{ cameraId: 'front-door', quality: 'auto', variantId: 'sub', liveAudioPreviewOffsetMs: -80 }
			]);
			const channel = FakePeerConnection.latest!.channels[0];
			if (videoOnly) {
				FakeDataChannel.audioVariants = [];
				channel.publishCapabilities();
			}
			await peer.configure([{ cameraId: 'front-door', quality: 'auto', variantId: 'main' }]);
			expect(peer.track('front-door')).toMatchObject({
				subscribed: true,
				liveAudioPreviewOffsetMs: undefined,
				audioVariantId: videoOnly ? undefined : 'sub',
				timingReconciliationNeeded: false
			});
			if (videoOnly) expect(channel.unsubscribed.flat()).toContain('audio-0');
			expect(channel.unsubscribed.flat()).not.toContain('camera-0');
			await peer.close();
		}
	);

	it('restores saved timing or cancels a pending preview when recovering a disconnected peer', async () => {
		for (const cancel of [false, true]) {
			vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
			api.createSession
				.mockResolvedValueOnce({ session_id: 'before', answer: { type: 'answer', sdp: 'v=0' } })
				.mockResolvedValueOnce({ session_id: 'after', answer: { type: 'answer', sdp: 'v=0' } });
			api.deleteSession.mockResolvedValue(undefined);
			FakeDataChannel.audioVariants = ['main', 'sub'];
			const peer = new LivePeer();
			await peer.configure([
				{
					cameraId: 'front-door',
					quality: 'auto',
					variantId: 'main',
					liveAudioPreviewOffsetMs: -80
				}
			]);
			FakeDataChannel.audioVariants = ['sub'];
			FakePeerConnection.latest!.channels[0].close();
			expect(peer.track('front-door')).toBeNull();
			if (cancel) peer.cancelDisconnectedPreview('front-door');
			await vi.waitFor(() =>
				expect(peer.track('front-door')).toMatchObject({
					subscribed: true,
					audioVariantId: 'sub',
					requestedVariantId: null,
					liveAudioPreviewOffsetMs: undefined,
					restoredSavedTiming: !cancel
				})
			);
			expect(peer.sessionId).toBe('after');
			const recoveredChannel = FakePeerConnection.latest!.channels[0];
			expect(recoveredChannel.unsubscribed.flat()).not.toContain('camera-0');
			const requests = recoveredChannel.subscriptions;
			expect(requests.some((request) => request.liveAudioPreviewOffsetMs === -80)).toBe(!cancel);
			await peer.close();
		}
	});

	it('does not confirm a reliable audio preview the browser cannot decode', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'timing',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['main'];
		FakeDataChannel.audioUsesReliable = true;
		vi.spyOn(AacAudioPlayback.prototype, 'configure').mockRejectedValue(
			new Error('decoder unavailable')
		);
		const peer = new LivePeer();
		await expect(
			peer.configure([
				{
					cameraId: 'front-door',
					variantId: 'main',
					quality: 'auto',
					liveAudioPreviewOffsetMs: -25
				}
			])
		).rejects.toThrow('cannot decode the audio timing preview');
		expect(api.deleteSession).toHaveBeenCalledOnce();
	});

	it('restores saved audio on the active profile after decoder failure, then follows its video handover', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'timing',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['main', 'sub'];
		FakeDataChannel.audioUsesReliable = true;
		vi.spyOn(AacAudioPlayback.prototype, 'configure')
			.mockResolvedValueOnce(undefined)
			.mockRejectedValueOnce(new Error('decoder unavailable'))
			.mockResolvedValue(undefined);
		const peer = new LivePeer();
		const saved = { cameraId: 'front-door', variantId: 'sub' as const, quality: 'auto' as const };
		await peer.configure([saved]);
		const channel = FakePeerConnection.latest!.channels[0];
		await expect(
			peer.configure([{ ...saved, variantId: 'main', liveAudioPreviewOffsetMs: -80 }])
		).rejects.toThrow('cannot decode the audio timing preview');
		const audio = () => channel.subscriptions.filter((s) => s.kind === MediaKind.AUDIO);
		expect(audio().map((s) => [s.variantId, s.liveAudioPreviewOffsetMs])).toEqual([
			['sub', undefined],
			['main', -80],
			['main', undefined]
		]);
		expect(peer.track('front-door')).toMatchObject({
			activeStream: 'main',
			pendingStream: 'sub',
			audioVariantId: 'main'
		});
		channel.onmessage?.(streamState('camera-0', 'sub'));
		await vi.waitFor(() =>
			expect(audio().map((s) => s.variantId)).toEqual(['sub', 'main', 'main', 'sub'])
		);
		expect(channel.unsubscribed.flat()).toEqual(['audio-0', 'audio-0']);
		expect(peer.track('front-door')).toMatchObject({ subscribed: true, audioVariantId: 'sub' });
		await peer.close();
	});

	it.each(['connected', 'recovered'] as const)(
		'follows a handover received before the saved audio response on a %s peer',
		async (mode) => {
			vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
			const answer = { type: 'answer', sdp: 'v=0' };
			api.createSession
				.mockResolvedValueOnce({ session_id: 'before', answer })
				.mockResolvedValue({ session_id: 'after', answer });
			api.deleteSession.mockResolvedValue(undefined);
			FakeDataChannel.audioVariants = ['main', 'sub'];
			FakeDataChannel.audioUsesReliable = true;
			vi.spyOn(AacAudioPlayback.prototype, 'configure')
				.mockResolvedValueOnce(undefined)
				.mockRejectedValueOnce(new Error('preview decoder unavailable'))
				.mockResolvedValue(undefined);
			const peer = new LivePeer();
			const saved = { cameraId: 'front-door', variantId: 'sub', quality: 'auto' } as const;
			const preview = { ...saved, variantId: 'main', liveAudioPreviewOffsetMs: -80 } as const;
			await peer.configure([mode === 'connected' ? saved : preview]);
			FakeDataChannel.holdNextSavedAudio = true;
			let rejectedPreview;
			if (mode === 'connected') {
				rejectedPreview = expect(peer.configure([preview])).rejects.toThrow(
					'cannot decode the audio timing preview'
				);
			} else {
				FakePeerConnection.latest!.channels[0].close();
			}
			await vi.waitFor(() =>
				expect(FakePeerConnection.latest!.channels[0].releaseSavedAudio).toBeDefined()
			);
			const channel = FakePeerConnection.latest!.channels[0];
			expect(peer.track(saved.cameraId)).toMatchObject({
				activeStream: 'main',
				pendingStream: 'sub'
			});
			const audio = () =>
				channel.subscriptions.filter((request) => request.kind === MediaKind.AUDIO);
			expect(audio().at(-1)?.variantId).toBe('main');
			expect(audio().at(-1)?.liveAudioPreviewOffsetMs).toBeUndefined();
			channel.onmessage?.(streamState('camera-0', 'sub'));
			channel.releaseSavedAudio!();
			await rejectedPreview;
			await vi.waitFor(() =>
				expect(peer.track(saved.cameraId)).toMatchObject({
					activeStream: 'sub',
					audioVariantId: 'sub',
					liveAudioPreviewOffsetMs: undefined,
					subscribed: true
				})
			);
			expect(audio().at(-1)?.variantId).toBe('sub');
			expect(audio().at(-1)?.liveAudioPreviewOffsetMs).toBeUndefined();
			expect(channel.unsubscribed.flat()).not.toContain('camera-0');
			await peer.close();
		}
	);

	it('keeps an unlocked audio context through reliable preview and saved binding replacements', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		const constructed = vi.fn();
		const closed = vi.fn();
		const resumed = vi.fn();
		vi.stubGlobal(
			'AudioContext',
			class {
				state = 'suspended';
				constructor() {
					constructed();
				}
				async resume() {
					this.state = 'running';
					resumed();
				}
				async close() {
					this.state = 'closed';
					closed();
				}
			}
		);
		api.createSession.mockResolvedValue({
			session_id: 'aac',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['main'];
		FakeDataChannel.audioUsesReliable = true;
		const configured = vi
			.spyOn(AacAudioPlayback.prototype, 'configure')
			.mockResolvedValue(undefined);
		const peer = new LivePeer();
		const saved = { cameraId: 'front-door', quality: 'auto' as const, variantId: 'main' as const };
		await peer.configure([saved]);
		await peer.resumeAudioPlayback();
		FakeDataChannel.audioBindingSuffix = '-preview';
		await peer.configure([{ ...saved, liveAudioPreviewOffsetMs: -25 }]);
		FakeDataChannel.audioBindingSuffix = '-reset';
		await peer.configure([saved]);
		await peer.resumeAudioPlayback();
		expect(configured).toHaveBeenCalledTimes(3);
		expect(constructed).toHaveBeenCalledTimes(1);
		expect(resumed).toHaveBeenCalledTimes(1);
		expect(closed).not.toHaveBeenCalled();
		await peer.close();
		expect(closed).toHaveBeenCalledTimes(1);
	});

	it('restores acknowledged saved timing and keeps video usable if this browser loses AAC support', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'timing',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['main'];
		FakeDataChannel.audioUsesReliable = true;
		const decoder = vi.spyOn(AacAudioPlayback.prototype, 'configure').mockResolvedValue(undefined);
		const peer = new LivePeer();
		const saved = { cameraId: 'front-door', quality: 'auto' as const, variantId: 'main' as const };
		await peer.configure([{ ...saved, liveAudioPreviewOffsetMs: 20 }]);
		decoder.mockRejectedValue(new Error('decoder unavailable'));
		// The server retires its reliable binding when returning to saved policy.
		FakeDataChannel.audioBindingSuffix = '-saved';
		await peer.configure([saved]);
		expect(peer.track('front-door')).toMatchObject({
			subscribed: true,
			audioTiming: { preview: false }
		});
		expect(peer.track('front-door')?.liveAudioPreviewOffsetMs).toBeUndefined();
		expect(decoder).toHaveBeenCalledTimes(2);
		await peer.configure([saved]);
		await peer.close();
	});

	it('keeps the video MID while awaiting the actual calibration keyframe handover', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'timing',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['main', 'sub'];
		const peer = new LivePeer();
		await peer.configure([{ cameraId: 'front-door', quality: 'auto' }]);
		const channel = FakePeerConnection.latest!.channels[0];
		FakeDataChannel.subscriptionQuality = create(LiveQualityDiagnosticsSchema, {
			activeVariantId: 'sub',
			pendingVariantId: 'main',
			selectionReason: LiveQualitySelectionReason.WAITING_KEYFRAME
		});
		const preview = peer.configure([
			{ cameraId: 'front-door', quality: 'auto', variantId: 'main', liveAudioPreviewOffsetMs: 45 }
		]);
		await vi.waitFor(() => expect(channel.subscriptions.at(-1)?.liveAudioPreviewOffsetMs).toBe(45));
		expect(channel.subscriptions.at(-1)?.kind).toBe(MediaKind.VIDEO);
		expect(peer.track('front-door')?.liveAudioPreviewOffsetMs).toBeUndefined();
		channel.onmessage?.(
			streamState(
				'camera-0',
				'main',
				create(LiveQualityDiagnosticsSchema, { activeVariantId: 'main' })
			)
		);
		await preview;
		expect(peer.track('front-door')).toMatchObject({
			confirmedVideoVariant: 'main',
			audioVariantId: 'main',
			liveAudioPreviewOffsetMs: 45
		});
		expect(channel.unsubscribed.flat()).not.toContain('camera-0');
		await peer.close();
	});

	it('keeps the admitted video and restores the saved plan after an unavailable preview', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'timing',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['main', 'sub'];
		const peer = new LivePeer();
		await peer.configure([{ cameraId: 'front-door', quality: 'auto' }]);
		FakeDataChannel.subscriptionQuality = create(LiveQualityDiagnosticsSchema, {
			activeVariantId: 'sub',
			selectionReason: LiveQualitySelectionReason.VARIANT_UNAVAILABLE
		});
		await expect(
			peer.configure([
				{
					cameraId: 'front-door',
					quality: 'auto',
					variantId: 'main',
					liveAudioPreviewOffsetMs: -80
				}
			])
		).rejects.toThrow('video profile is unavailable');
		expect(peer.track('front-door')).toMatchObject({
			subscribed: true,
			requestedVariantId: null,
			audioVariantId: 'sub'
		});
		expect(FakePeerConnection.latest!.channels[0].unsubscribed.flat()).not.toContain('camera-0');
		await peer.close();
	});

	it('reconciles an initially rejected audio track when the active profile becomes available', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'timing',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['sub'];
		FakeDataChannel.rejectedSubscription = 'audio-0';
		const peer = new LivePeer();
		await peer.configure([{ cameraId: 'front-door', quality: 'auto' }]);
		const channel = FakePeerConnection.latest!.channels[0];
		await vi.waitFor(() =>
			expect(
				channel.subscriptions.filter((s) => s.kind === MediaKind.AUDIO).length
			).toBeGreaterThan(1)
		);
		FakeDataChannel.rejectedSubscription = null;
		await vi.waitFor(() => expect(peer.track('front-door')?.audioVariantId).toBe('sub'), {
			timeout: 2000
		});
		expect(channel.subscriptions.filter((s) => s.kind === MediaKind.VIDEO)).toHaveLength(1);
		await peer.close();
	});

	it('reconciles a reversed profile switch while the first audio unsubscribe is pending', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'timing',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['main', 'sub'];
		const peer = new LivePeer();
		await peer.configure([{ cameraId: 'front-door', quality: 'auto' }]);
		const channel = FakePeerConnection.latest!.channels[0];
		channel.holdUnsubscribe = true;
		channel.onmessage?.(streamState('camera-0', 'main'));
		await vi.waitFor(() => expect(channel.releaseUnsubscribe).toBeDefined());
		channel.onmessage?.(streamState('camera-0', 'sub'));
		channel.releaseUnsubscribe!();
		await vi.waitFor(() =>
			expect(
				channel.subscriptions.filter((s) => s.kind === MediaKind.AUDIO).map((s) => s.variantId)
			).toEqual(['sub', 'main', 'sub'])
		);
		expect(peer.track('front-door')?.audioVariantId).toBe('sub');
		await peer.close();
	});

	it.each(['camera-0', 'audio-0'])(
		'keeps preview active when reset admission fails on %s',
		async (id) => {
			vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
			api.createSession.mockResolvedValue({
				session_id: 'timing',
				answer: { type: 'answer', sdp: 'v=0' }
			});
			api.deleteSession.mockResolvedValue(undefined);
			FakeDataChannel.audioVariants = ['sub'];
			const plan = { cameraId: 'front-door', quality: 'auto', variantId: 'sub' } as const;
			const peer = new LivePeer();
			await peer.configure([{ ...plan, liveAudioPreviewOffsetMs: -90 }]);
			FakeDataChannel.rejectedSubscription = id;
			await expect(peer.configure([plan])).rejects.toThrow('timing preview was not admitted');
			expect(peer.track('front-door')?.liveAudioPreviewOffsetMs).toBe(-90);
			FakeDataChannel.rejectedSubscription = null;
			await peer.configure([plan]);
			expect(peer.track('front-door')?.liveAudioPreviewOffsetMs).toBeUndefined();
			await peer.close();
		}
	);

	it.each([
		LiveQualitySelectionReason.WAITING_KEYFRAME,
		LiveQualitySelectionReason.VARIANT_UNAVAILABLE
	])(
		'follows the active video when a pinned Main profile is pending or unavailable (%s)',
		async (reason) => {
			vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
			api.createSession.mockResolvedValue({
				session_id: 'pinned',
				answer: { type: 'answer', sdp: 'v=0' }
			});
			api.deleteSession.mockResolvedValue(undefined);
			FakeDataChannel.audioVariants = ['main', 'sub'];
			const retained = create(LiveQualityDiagnosticsSchema, {
				activeVariantId: 'sub',
				pendingVariantId: reason === LiveQualitySelectionReason.WAITING_KEYFRAME ? 'main' : '',
				selectionReason: reason
			});
			FakeDataChannel.subscriptionQuality = retained;
			const peer = new LivePeer();
			try {
				await peer.configure([{ cameraId: 'front-door', quality: 'high', variantId: 'main' }]);
				const channel = FakePeerConnection.latest!.channels[0];
				const audio = () =>
					channel.subscriptions.filter((s) => s.kind === MediaKind.AUDIO).map((s) => s.variantId);
				expect(audio()).toEqual(['sub']);
				channel.onmessage?.(
					streamState(
						'camera-0',
						'main',
						create(LiveQualityDiagnosticsSchema, { activeVariantId: 'main' })
					)
				);
				await vi.waitFor(() => expect(audio()).toEqual(['sub', 'main']));
				channel.onmessage?.(streamState('camera-0', 'sub', retained));
				await vi.waitFor(() => expect(audio()).toEqual(['sub', 'main', 'sub']));
				expect(peer.track('front-door')).toMatchObject({
					requestedVariantId: 'main',
					activeStream: 'sub',
					audioVariantId: 'sub'
				});
			} finally {
				await peer.close();
			}
		}
	);

	it('follows the active video profile after an adaptive handover', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'timing',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['main', 'sub'];
		const peer = new LivePeer();
		await peer.configure([{ cameraId: 'front-door', quality: 'auto' }]);
		expect(peer.track('front-door')?.audioVariantId).toBe('sub');
		const channel = FakePeerConnection.latest!.channels[0];
		channel.onmessage?.(streamState('camera-0', 'main'));
		await vi.waitFor(() => expect(peer.track('front-door')?.audioVariantId).toBe('main'));
		expect(channel.commands.slice(-2)).toEqual(['unsubscribe', 'subscribeMedia']);
		await peer.close();
	});

	it('uses actual active and pending diagnostics instead of treating the selected target as active', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'quality',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const quality = create(LiveQualityDiagnosticsSchema, {
			sessionBandwidthBps: 8_000_000n,
			allocatedBitrateBps: 0n,
			activeVariantId: 'main',
			pendingVariantId: 'sub',
			selectionReason: LiveQualitySelectionReason.WAITING_KEYFRAME
		});
		FakeDataChannel.subscriptionQuality = quality;
		const peer = new LivePeer();
		await peer.configure([{ cameraId: 'front-door', quality: 'auto' }]);
		expect(peer.track('front-door')).toMatchObject({
			activeStream: 'main',
			pendingStream: 'sub',
			requestedQuality: 'auto',
			liveQuality: quality
		});
		expect(peer.estimatedBitrateBps).toBe(8_000_000);
		await peer.close();
	});

	it('scopes rich updates to admitted video subscriptions and retires diagnostics on unsubscribe', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'quality',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		await peer.configure([{ cameraId: 'front-door', quality: 'auto' }]);
		const channel = FakePeerConnection.latest!.channels[0];
		const quality = create(LiveQualityDiagnosticsSchema, {
			sessionBandwidthBps: 4_000_000n,
			allocatedBitrateBps: 900_000n,
			activeVariantId: 'sub',
			pendingVariantId: 'main',
			selectionReason: LiveQualitySelectionReason.UPGRADE_HOLD
		});
		const original = peer.track('front-door');
		channel.onmessage?.(streamState('audio-0', 'main', quality));
		expect(peer.track('front-door')).toBe(original);
		channel.onmessage?.(streamState('camera-0', 'main', quality));
		expect(peer.track('front-door')).toMatchObject({
			activeStream: 'sub',
			pendingStream: 'main',
			liveQuality: quality
		});
		channel.onmessage?.(
			streamState(
				'camera-0',
				'main',
				create(LiveQualityDiagnosticsSchema, {
					...quality,
					activeVariantId: 'main',
					pendingVariantId: '',
					selectionReason: LiveQualitySelectionReason.STABLE
				})
			)
		);
		expect(peer.track('front-door')).toMatchObject({ activeStream: 'main', pendingStream: null });
		await peer.configure([{ cameraId: 'front-door', quality: 'auto', active: false }]);
		expect(peer.track('front-door')?.liveQuality).toBeUndefined();
		const retired = peer.track('front-door');
		channel.onmessage?.(streamState('camera-0', 'sub', quality));
		expect(peer.track('front-door')).toBe(retired);
		await peer.close();
	});

	it('preserves a known stream on unfamiliar variants and clears stale evidence for legacy and removed sources', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'quality',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		await peer.configure([{ cameraId: 'front-door', quality: 'high', variantId: 'main' }]);
		const channel = FakePeerConnection.latest!.channels[0];
		channel.onmessage?.(streamState('camera-0', 'main'));
		const future = create(LiveQualityDiagnosticsSchema, {
			activeVariantId: 'future-variant',
			pendingVariantId: '',
			selectionReason: 999 as LiveQualitySelectionReason
		});
		channel.onmessage?.(streamState('camera-0', 'future-variant', future));
		expect(peer.track('front-door')).toMatchObject({
			activeStream: 'main',
			pendingStream: null,
			liveQuality: future
		});
		channel.onmessage?.(streamState('camera-0', 'main'));
		expect(peer.track('front-door')?.liveQuality).toBeUndefined();
		channel.onmessage?.(
			streamState(
				'camera-0',
				'main',
				create(LiveQualityDiagnosticsSchema, { activeVariantId: 'main' })
			)
		);
		channel.onmessage?.(
			notification({
				case: 'sourceSessionRemoved',
				value: create(SourceSessionRemovedSchema, { sourceSessionId: 'camera:front-door' })
			})
		);
		expect(peer.track('front-door')?.liveQuality).toBeUndefined();
		await peer.close();
	});

	it('rejects an unfamiliar initially selected variant instead of guessing Sub', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'quality',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.selectedVariant = 'future-variant';
		const peer = new LivePeer();
		await expect(peer.configure([{ cameraId: 'front-door', quality: 'auto' }])).rejects.toThrow(
			'unsupported live variant'
		);
		expect(peer.tracks).toEqual({});
		expect(api.deleteSession).toHaveBeenCalledWith('quality', null, undefined);
	});

	it('keeps network estimates session-wide and preserves zero without unsafe conversion', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'network',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		await peer.configure([
			{ cameraId: 'front-door', quality: 'auto' },
			{ cameraId: 'garage', quality: 'high', variantId: 'main' }
		]);
		const channel = FakePeerConnection.latest!.channels[0];
		const manualTrack = peer.track('garage');
		expect(peer.estimatedBitrateBps).toBeNull();
		for (const [reported, expected] of [
			[8_000_000n, 8_000_000],
			[0n, 0],
			[BigInt(Number.MAX_SAFE_INTEGER), Number.MAX_SAFE_INTEGER],
			[BigInt(Number.MAX_SAFE_INTEGER) + 1n, null],
			[18_446_744_073_709_551_615n, null]
		] as const) {
			channel.onmessage?.(networkEstimate(reported));
			expect(peer.estimatedBitrateBps).toBe(expected);
		}
		expect(peer.track('front-door')?.estimatedBitrateBps).toBeNull();
		expect(peer.track('garage')).toMatchObject({
			requestedQuality: 'high',
			requestedVariantId: 'main',
			pendingStream: 'main'
		});
		expect(peer.track('garage')).toBe(manualTrack);
		expect(channel.commands).toEqual(['subscribeMedia', 'subscribeMedia']);
		await peer.close();
		expect(peer.estimatedBitrateBps).toBeNull();
	});

	it('resets network estimates on replacement and ignores messages from abandoned peers', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'network',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		await peer.configure([{ cameraId: 'front-door', quality: 'auto' }]);
		const abandoned = FakePeerConnection.latest!.channels[0].onmessage!;
		abandoned(networkEstimate(8_000_000n));
		expect(peer.estimatedBitrateBps).toBe(8_000_000);
		await peer.configure([{ cameraId: 'garage', quality: 'high' }]);
		expect(peer.estimatedBitrateBps).toBeNull();
		abandoned(networkEstimate(9_000_000n));
		expect(peer.estimatedBitrateBps).toBeNull();
		const current = FakePeerConnection.latest!.channels[0].onmessage!;
		current(networkEstimate(3_000_000n));
		expect(peer.estimatedBitrateBps).toBe(3_000_000);
		peer.closeOnPageHide();
		expect(peer.estimatedBitrateBps).toBeNull();
		current(networkEstimate(4_000_000n));
		expect(peer.estimatedBitrateBps).toBeNull();
	});

	it('renegotiates current camera capabilities after losing a shared peer without replaying a command', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession
			.mockResolvedValueOnce({ session_id: 'first', answer: { type: 'answer', sdp: 'v=0' } })
			.mockResolvedValueOnce({ session_id: 'replacement', answer: { type: 'answer', sdp: 'v=0' } });
		api.deleteSession.mockResolvedValue(undefined);
		FakeDataChannel.audioVariants = ['main', 'sub'];
		const peer = new LivePeer();
		const plans = [
			{ cameraId: 'front-door', quality: 'auto', variantId: 'sub', liveAudioPreviewOffsetMs: -80 },
			{ cameraId: 'garage', quality: 'low' }
		] as const;
		await peer.configure([...plans]);
		const old = FakePeerConnection.latest!;
		old.channels[0].holdCameraCommand = true;
		const pending = peer.playChime('front-door', 'bell');
		const rejected = expect(pending).rejects.toThrow(/closed|changed/);
		await vi.waitFor(() => expect(old.channels[0].commands).toContain('cameraControlCommand'));
		FakeDataChannel.rejectedSubscription = 'camera-1';
		old.connectionState = 'disconnected';
		old.onconnectionstatechange?.();
		await rejected;
		await vi.waitFor(() => expect(peer.sessionId).toBe('replacement'));
		expect(api.createSession).toHaveBeenLastCalledWith(
			expect.objectContaining({ type: 'offer' }),
			null,
			'first'
		);
		expect(peer.track('front-door')?.subscribed).toBe(true);
		expect(peer.track('garage')?.subscribed).toBe(false);
		const fresh = FakePeerConnection.latest!;
		expect(peer.talkbackError).toMatch(/check whether it ran/i);
		expect(fresh).not.toBe(old);
		expect(
			fresh.channels[0].subscriptions
				.filter((subscription) => subscription.liveAudioPreviewOffsetMs === -80)
				.map((subscription) => [subscription.kind, subscription.variantId])
		).toEqual([
			[MediaKind.VIDEO, 'sub'],
			[MediaKind.AUDIO, 'sub']
		]);
		expect(fresh.channels[0].commands).not.toContain('cameraControlCommand');
		await peer.playChime('front-door', 'bell');
		expect(fresh.channels[0].commands.filter((c) => c === 'cameraControlCommand')).toHaveLength(1);
		await peer.close();
	});

	it.each(['connection', 'ice'] as const)(
		'clears the session estimate immediately on %s disconnect before asynchronous teardown',
		async (kind) => {
			vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
			api.createSession.mockResolvedValue({
				session_id: 'network',
				answer: { type: 'answer', sdp: 'v=0' }
			});
			api.deleteSession.mockResolvedValue(undefined);
			const peer = new LivePeer();
			await peer.configure([{ cameraId: 'front-door', quality: 'auto' }]);
			const connection = FakePeerConnection.latest!;
			connection.channels[0].onmessage?.(networkEstimate(8_000_000n));
			expect(peer.estimatedBitrateBps).toBe(8_000_000);
			if (kind === 'connection') {
				connection.connectionState = 'disconnected';
				connection.onconnectionstatechange?.();
			} else {
				connection.iceConnectionState = 'disconnected';
				connection.oniceconnectionstatechange?.();
			}
			expect(peer.estimatedBitrateBps).toBeNull();
			connection.channels[0].onmessage?.(networkEstimate(9_000_000n));
			expect(peer.estimatedBitrateBps).toBeNull();
			await peer.close();
		}
	);

	it('retries the latest plan when its replacement signaling attempt fails during recovery', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		const response = { session_id: 'first', answer: { type: 'answer', sdp: 'v=0' } };
		api.createSession
			.mockResolvedValueOnce(response)
			.mockRejectedValueOnce(new ApiRequestError(503, 'temporarily unavailable'))
			.mockRejectedValueOnce(new ApiRequestError(503, 'still unavailable'))
			.mockResolvedValueOnce({ ...response, session_id: 'restored' });
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		await peer.configure([{ cameraId: 'front-door', quality: 'low' }]);
		vi.useFakeTimers();
		try {
			FakePeerConnection.latest!.channels[0].close();
			await vi.advanceTimersByTimeAsync(1);
			await expect(peer.configure([{ cameraId: 'front-door', quality: 'high' }])).rejects.toThrow(
				'still unavailable'
			);
			await vi.runAllTimersAsync();
			expect(peer.sessionId).toBe('restored');
			expect(peer.track('front-door')?.requestedQuality).toBe('high');
		} finally {
			vi.useRealTimers();
			await peer.close();
		}
	});

	it('recovers while the old microphone prompt is open and stops a late microphone stream', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		let answer!: (stream: MediaStream) => void;
		const microphone = vi.spyOn(navigator.mediaDevices, 'getUserMedia').mockImplementation(
			() =>
				new Promise((resolve) => {
					answer = resolve;
				})
		);
		const response = { session_id: 'first', answer: { type: 'answer', sdp: 'v=0' } };
		api.createSession
			.mockResolvedValueOnce(response)
			.mockResolvedValueOnce({ ...response, session_id: 'restored' });
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		await peer.configure([{ cameraId: 'front-door', quality: 'low' }]);
		const command = peer.startTalkback({ all: true }).catch((error: Error) => error);
		await vi.waitFor(() => expect(microphone).toHaveBeenCalledOnce());
		FakePeerConnection.latest!.channels[0].close();
		expect(await command).toMatchObject({ message: expect.stringMatching(/connection changed/i) });
		await vi.waitFor(() => expect(peer.sessionId).toBe('restored'));
		const stop = vi.fn();
		answer({ getTracks: () => [{ stop }] } as unknown as MediaStream);
		await vi.waitFor(() => expect(stop).toHaveBeenCalledOnce());
		expect(peer.talkbackActive).toBe(false);
		await peer.close();
	});

	it('publishes live command errors before connecting', async () => {
		const peer = new LivePeer();
		await expect(peer.playChime('front-door', 'bell')).rejects.toThrow(/connect to live view/i);
		expect(peer.error).toBeNull();
		expect(peer.talkbackError).toMatch(/connect to live view/i);
	});

	it('releases an intentionally closed peer before remote delete and opens the next viewer without a continuation', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		const response = { session_id: 'intentional', answer: { type: 'answer', sdp: 'v=0' } };
		api.createSession.mockResolvedValue(response);
		const peer = new LivePeer();
		const plan = { cameraId: 'front-door', quality: 'auto' } as const;
		await peer.configure([plan]);
		const old = FakePeerConnection.latest!;
		api.deleteSession.mockImplementationOnce(async () => {
			expect(peer.sessionId).toBeNull();
			old.channels[0].close();
		});
		await peer.configure([plan, { cameraId: 'garage', quality: 'low' }]);
		expect(api.createSession).toHaveBeenLastCalledWith(expect.anything(), null, undefined);
		await peer.close();
	});

	it('forgets a refused privacy continuation and waits for a new authorized configure', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		const response = { session_id: 'initial', answer: { type: 'answer', sdp: 'v=0' } };
		api.createSession
			.mockResolvedValueOnce(response)
			.mockRejectedValueOnce(new ApiRequestError(401, 'ordinary disconnect'))
			.mockResolvedValueOnce({ ...response, session_id: 'authorized' });
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		const plans = [{ cameraId: 'front-door', quality: 'auto' }] as const;
		await peer.configure([...plans]);
		FakePeerConnection.latest!.channels[0].close();
		await vi.waitFor(() =>
			expect(api.deleteSession).toHaveBeenCalledWith('initial', null, undefined)
		);
		expect(api.createSession).toHaveBeenCalledTimes(2);
		expect(peer.sessionId).toBeNull();
		await peer.configure([...plans]);
		expect(api.createSession).toHaveBeenLastCalledWith(expect.anything(), null, undefined);
		expect(peer.sessionId).toBe('authorized');
		await peer.close();
	});

	it('consumes an acknowledged continuation even if its opening media peer was displaced', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		const response = { session_id: 'original', answer: { type: 'answer', sdp: 'v=0' } };
		const accepted = Promise.withResolvers<typeof response>();
		api.createSession
			.mockResolvedValueOnce(response)
			.mockReturnValueOnce(accepted.promise)
			.mockResolvedValueOnce({ ...response, session_id: 'fresh' });
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		await peer.configure([{ cameraId: 'front-door', quality: 'auto' }]);
		FakePeerConnection.latest!.channels[0].close();
		await vi.waitFor(() => expect(api.createSession).toHaveBeenCalledTimes(2));
		FakePeerConnection.latest!.channels[0].close();
		accepted.resolve({ ...response, session_id: 'unused' });
		await vi.waitFor(() => expect(peer.sessionId).toBe('fresh'));
		expect(api.deleteSession).toHaveBeenCalledWith('unused', null, undefined);
		expect(api.createSession).toHaveBeenLastCalledWith(expect.anything(), null, undefined);
		await peer.close();
	});

	it.each(['main', 'sub'] as const)(
		'reconciles a refused pinned %s audio track without replacing admitted video',
		async (variant) => {
			vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
			api.createSession.mockResolvedValue({
				session_id: 'partial',
				answer: { type: 'answer', sdp: 'v=0' }
			});
			api.deleteSession.mockResolvedValue(undefined);
			FakeDataChannel.audioVariants = ['main', 'sub'];
			FakeDataChannel.subscriptionQuality = create(LiveQualityDiagnosticsSchema, {
				activeVariantId: variant
			});
			FakeDataChannel.rejectedSubscription = 'audio-0';
			const peer = new LivePeer();
			const plans = [{ cameraId: 'front-door', quality: 'auto', variantId: variant }] as const;
			await peer.configure([...plans]);
			expect(peer.track('front-door')?.subscribed).toBe(true);
			expect(peer.needsAdmission('front-door')).toBe(true);
			FakeDataChannel.rejectedSubscription = null;
			await peer.configure([...plans]);
			await vi.waitFor(() => expect(peer.needsAdmission('front-door')).toBe(false));
			expect(
				FakePeerConnection.latest!.channels[0].subscriptions.map((s) => s.subscriptionId)
			).toEqual(['camera-0', 'audio-0', 'audio-0']);
			expect(FakePeerConnection.latest!.channels[0].subscriptions.at(-1)?.variantId).toBe(variant);
			await peer.close();
		}
	);

	it('recovers immediately if the signaled control channel is retired before opening', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		FakePeerConnection.holdNextControlOpen = true;
		const response = { session_id: 'opening', answer: { type: 'answer', sdp: 'v=0' } };
		api.createSession
			.mockResolvedValueOnce(response)
			.mockResolvedValueOnce({ ...response, session_id: 'restored' });
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		const opening = expect(
			peer.configure([{ cameraId: 'front-door', quality: 'low' }])
		).rejects.toThrow(/closed before opening/);
		await vi.waitFor(() => expect(peer.sessionId).toBe('opening'));
		FakePeerConnection.latest!.channels[0].close();
		await opening;
		await vi.waitFor(() => expect(peer.track('front-door')?.subscribed).toBe(true));
		expect(peer.sessionId).toBe('restored');
		await peer.close();
	});

	it('bounds repeated replacements which open successfully and then immediately close', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'flapping',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		const plans = [{ cameraId: 'front-door', quality: 'low' as const }];
		await peer.configure(plans);
		vi.useFakeTimers();
		try {
			for (const delay of [0, 750, 2000]) {
				FakePeerConnection.latest!.channels[0].close();
				const before = api.createSession.mock.calls.length;
				for (let pulse = 0; pulse < 4; pulse++) await peer.configure(plans, 'reconcile');
				expect(api.createSession).toHaveBeenCalledTimes(before);
				await vi.advanceTimersByTimeAsync(delay);
				expect(peer.sessionId).toBe('flapping');
			}
			FakePeerConnection.latest!.channels[0].close();
			await vi.runAllTimersAsync();
			expect(api.createSession).toHaveBeenCalledTimes(4);
			expect(peer.error).toMatch(/reopen/i);
			for (let pulse = 0; pulse < 5; pulse++) {
				await peer.configure(plans, 'reconcile');
				await vi.advanceTimersByTimeAsync(5000);
			}
			expect(api.createSession).toHaveBeenCalledTimes(4);
			peer.resumeRecovery();
			await vi.runAllTimersAsync();
			expect(api.createSession).toHaveBeenCalledTimes(5);
		} finally {
			vi.useRealTimers();
			await peer.close();
		}
	});

	it.each([403, 503])(
		'rechecks authorization and bounds retries after signaling fails with %i',
		async (status) => {
			vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
			api.createSession
				.mockResolvedValueOnce({ session_id: 'first', answer: { type: 'answer', sdp: 'v=0' } })
				.mockRejectedValue(new ApiRequestError(status, 'signaling refused'));
			api.deleteSession.mockResolvedValue(undefined);
			const peer = new LivePeer();
			await peer.configure([{ cameraId: 'front-door', quality: 'auto' }]);
			vi.useFakeTimers();
			try {
				FakePeerConnection.latest!.channels[0].close();
				await vi.runAllTimersAsync();
				expect(api.createSession).toHaveBeenCalledTimes(status === 403 ? 2 : 4);
				expect(peer.sessionId).toBeNull();
				expect(vi.getTimerCount()).toBe(0);
			} finally {
				vi.useRealTimers();
				await peer.close();
			}
		}
	);

	it('keeps automatic plan refreshes stopped after initial denial until authorization is confirmed', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession
			.mockRejectedValueOnce(new ApiRequestError(403, 'denied'))
			.mockResolvedValue({ session_id: 'authorized', answer: { type: 'answer', sdp: 'v=0' } });
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		const plans = [{ cameraId: 'front-door', quality: 'auto' as const }];
		await expect(peer.configure(plans, 'reconcile')).rejects.toThrow('denied');
		for (let pulse = 0; pulse < 4; pulse++) await peer.configure(plans, 'reconcile');
		expect(api.createSession).toHaveBeenCalledOnce();
		peer.resumeRecovery();
		await vi.waitFor(() => expect(peer.sessionId).toBe('authorized'));
		expect(api.createSession).toHaveBeenCalledTimes(2);
		await peer.close();
	});

	it('leaves no channel-opening timer when signaling rejects revoked access', async () => {
		vi.useFakeTimers();
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockRejectedValue(new Error('Access was revoked'));
		const peer = new LivePeer();
		try {
			await expect(peer.configure([{ cameraId: 'front-door', quality: 'low' }])).rejects.toThrow(
				'Access was revoked'
			);
			expect(peer.connectionState).toBe('closed');
			expect(peer.error).toBe('Access was revoked');
			expect(vi.getTimerCount()).toBe(0);
		} finally {
			vi.clearAllTimers();
			vi.useRealTimers();
		}
	});

	it('leaves no channel-opening timer when an offer is abandoned', async () => {
		vi.useFakeTimers();
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		const peer = new LivePeer();
		vi.spyOn(FakePeerConnection.prototype, 'createOffer').mockImplementation(async () => {
			peer.closeOnPageHide();
			return { type: 'offer', sdp: 'v=0' };
		});
		try {
			await peer.configure([{ cameraId: 'front-door', quality: 'low' }]);
			expect(api.createSession).not.toHaveBeenCalled();
			expect(peer.connectionState).toBe('closed');
			expect(vi.getTimerCount()).toBe(0);
		} finally {
			vi.clearAllTimers();
			vi.useRealTimers();
		}
	});

	it('still rejects and cleans up when a signaled control channel never opens', async () => {
		vi.useFakeTimers();
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		vi.spyOn(FakePeerConnection.prototype, 'setRemoteDescription').mockResolvedValue(undefined);
		api.createSession.mockResolvedValue({
			session_id: 'never-opened',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		try {
			const rejected = expect(
				peer.configure([{ cameraId: 'front-door', quality: 'low' }])
			).rejects.toThrow('WebRTC control-channel channel did not open.');
			await vi.advanceTimersByTimeAsync(10_000);
			await rejected;
			expect(api.deleteSession).toHaveBeenCalledWith('never-opened', null, undefined);
			expect(peer.connectionState).toBe('closed');
			expect(vi.getTimerCount()).toBe(0);
		} finally {
			vi.clearAllTimers();
			vi.useRealTimers();
		}
	});

	it('retains recovery errors for an authorized camera with no live source yet', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		FakeDataChannel.offlineCamera = 'offline-camera';
		api.createSession.mockResolvedValue({
			session_id: 'offline',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		await expect(peer.configure([{ cameraId: 'offline-camera', quality: 'low' }])).rejects.toThrow(
			'no live source'
		);
		expect(api.deleteSession).toHaveBeenCalledWith('offline', null, undefined);
	});

	it('keeps an unadvertised camera unavailable without subscribing or failing other tracks', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'restricted',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		await expect(
			peer.configure([
				{ cameraId: 'front-door', quality: 'low' },
				{ cameraId: 'hidden-camera', quality: 'low' }
			])
		).resolves.toBeUndefined();
		expect(peer.track('front-door')?.subscribed).toBe(true);
		expect(peer.track('hidden-camera')).toMatchObject({ status: 'unavailable', subscribed: false });
		expect(peer.error).toBeNull();
		expect(FakePeerConnection.latest?.channels[0].commands).toEqual(['subscribeMedia']);
		await peer.close();
	});

	it('shares one negotiated session and tears it down after unsubscribe', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: '42',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();

		await peer.configure([
			{ cameraId: 'front-door', quality: 'low' },
			{ cameraId: 'garage', quality: 'high', variantId: 'main' }
		]);

		expect(peer.sessionId).toBe('42');
		expect(Object.keys(peer.tracks)).toEqual(['front-door', 'garage']);
		expect(peer.track('front-door')).toMatchObject({
			status: 'connecting',
			requestedQuality: 'low',
			requestedVariantId: null,
			subscribed: true
		});
		expect(peer.track('garage')).toMatchObject({
			requestedQuality: 'high',
			requestedVariantId: 'main',
			subscribed: true
		});
		expect(
			FakePeerConnection.latest?.channels.map(({ label, options }) => [label, options])
		).toEqual([
			['control-channel', { negotiated: true, id: 0, ordered: true }],
			['reliable-data', { negotiated: true, id: 1, ordered: true }],
			['unreliable-data', { negotiated: true, id: 2, ordered: false, maxRetransmits: 0 }]
		]);

		await peer.close();

		expect(api.deleteSession).toHaveBeenCalledWith('42', null, undefined);
		expect(peer.sessionId).toBeNull();
		expect(peer.tracks).toEqual({});
		expect(peer.connectionState).toBe('closed');
	});

	it('keeps an unattached session alive while held and closes after release', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'held-session',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		const detach = peer.attach('front-door');
		const release = peer.hold();
		await peer.configure([{ cameraId: 'front-door', quality: 'auto' }]);

		detach();
		await Promise.resolve();
		expect(api.deleteSession).not.toHaveBeenCalled();

		release();
		await vi.waitFor(() =>
			expect(api.deleteSession).toHaveBeenCalledWith('held-session', null, undefined)
		);
		expect(peer.tracks).toEqual({});
	});

	it('preserves the negotiated receiver while delivery is pulsed', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'pulse-session',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		const peer = new LivePeer();
		const plan = { cameraId: 'front-door', quality: 'low' as const, variantId: 'sub' as const };

		await peer.configure([plan]);
		const connection = FakePeerConnection.latest;
		const receiver = {} as RTCRtpReceiver;
		const stream = new MediaStream();
		connection?.ontrack?.({
			transceiver: connection.transceivers[0],
			receiver,
			track: null,
			streams: [stream]
		} as unknown as RTCTrackEvent);
		expect(peer.track('front-door')).toMatchObject({ receiver, stream, subscribed: true });

		await peer.configure([{ ...plan, active: false }]);
		expect(peer.track('front-door')).toMatchObject({
			receiver,
			stream,
			status: 'queued',
			subscribed: false
		});

		await peer.configure([plan]);
		expect(peer.track('front-door')).toMatchObject({ receiver, stream, subscribed: true });
	});

	it('releases an old subscription before admitting its replacement', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'bounded-handoff',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		await peer.configure([
			{ cameraId: 'front-door', quality: 'low', active: false },
			{ cameraId: 'garage', quality: 'low', active: true }
		]);
		await peer.configure([
			{ cameraId: 'front-door', quality: 'low', active: true },
			{ cameraId: 'garage', quality: 'low', active: false }
		]);
		expect(FakePeerConnection.latest?.channels[0].commands).toEqual([
			'subscribeMedia',
			'unsubscribe',
			'subscribeMedia'
		]);
		expect(peer.track('front-door')?.subscribed).toBe(true);
		expect(peer.track('garage')?.subscribed).toBe(false);
		expect(peer.sessionId).toBe('bounded-handoff');
		await peer.close();
	});

	it('reports a refused stream without closing the other admitted streams', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		FakeDataChannel.rejectedSubscription = 'camera-1';
		api.createSession.mockResolvedValue({
			session_id: 'limited-session',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		await peer.configure([
			{ cameraId: 'front-door', quality: 'low' },
			{ cameraId: 'garage', quality: 'low' }
		]);
		expect(peer.track('front-door')?.subscribed).toBe(true);
		expect(peer.track('garage')).toMatchObject({
			status: 'unavailable',
			subscribed: false,
			admissionError: 'Server did not admit this stream'
		});
		expect(peer.sessionId).toBe('limited-session');
		expect(api.deleteSession).not.toHaveBeenCalled();
		await peer.close();
	});

	it('stops local media when an eviction cannot be confirmed', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		api.createSession.mockResolvedValue({
			session_id: 'failed-eviction',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();
		await peer.configure([{ cameraId: 'front-door', quality: 'low' }]);
		FakeDataChannel.rejectUnsubscribe = true;
		await expect(
			peer.configure([{ cameraId: 'front-door', quality: 'low', active: false }])
		).rejects.toThrow();
		expect(peer.tracks).toEqual({});
		expect(peer.sessionId).toBeNull();
		expect(peer.error).toBe('Unable to release live streams');
		expect(api.deleteSession).toHaveBeenCalledWith('failed-eviction', null, undefined);
	});

	it('deletes a created server session when remote setup fails', async () => {
		vi.stubGlobal('RTCPeerConnection', FakePeerConnection);
		FakeDataChannel.suppressCapabilities = true;
		FakePeerConnection.failRemoteDescription = true;
		api.createSession.mockResolvedValue({
			session_id: 'failed-session',
			answer: { type: 'answer', sdp: 'v=0' }
		});
		api.deleteSession.mockResolvedValue(undefined);
		const peer = new LivePeer();

		await expect(peer.configure([{ cameraId: 'front-door', quality: 'low' }])).rejects.toThrow(
			'remote description rejected'
		);

		expect(api.deleteSession).toHaveBeenCalledWith('failed-session', null, undefined);
		expect(peer.error).toBe('remote description rejected');
		expect(peer.sessionId).toBeNull();
		expect(peer.tracks).toEqual({});
	});
});
