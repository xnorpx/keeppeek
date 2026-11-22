import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	CameraInfoSchema,
	ConnectionUpdateSchema,
	ControlEnvelopeSchema,
	LiveQualityDiagnosticsSchema,
	LiveQualitySelectionReason,
	MediaStreamCapabilitySchema,
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

vi.mock('./api', () => api);

import { LivePeer } from './stream-peer.svelte';

class FakeDataChannel {
	static suppressCapabilities = false;
	static offlineCamera: string | null = null;
	static rejectedSubscription: string | null = null;
	static rejectUnsubscribe = false;
	static subscriptionQuality: LiveQualityDiagnostics | undefined;
	static selectedVariant: string | null = null;

	readyState: RTCDataChannelState = 'connecting';
	binaryType: BinaryType = 'blob';
	onopen: (() => void) | null = null;
	onclose: (() => void) | null = null;
	onerror: (() => void) | null = null;
	onmessage: ((event: MessageEvent) => void) | null = null;
	readonly commands: string[] = [];

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

		const result =
			command.case === 'subscribeMedia'
				? {
						case: 'subscriptionResult' as const,
						value: create(SubscriptionResultSchema, {
							subscriptionId: command.value.subscriptionId,
							delivery: {
								case: 'rtp',
								value: create(RtpDeliverySchema, {
									mid: command.value.subscriptionId.replace('camera-', '')
								})
							},
							selectedVariantId:
								FakeDataChannel.selectedVariant ?? (command.value.variantId || 'sub'),
							liveQuality: FakeDataChannel.subscriptionQuality
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
		const encoded = toBinary(ControlEnvelopeSchema, response);
		queueMicrotask(() => this.onmessage?.({ data: encoded.buffer } as MessageEvent));
	}

	open(): void {
		this.readyState = 'open';
		this.onopen?.();
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
									video: create(MediaStreamCapabilitySchema)
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
		for (const channel of this.channels) channel.open();
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
	FakeDataChannel.offlineCamera = null;
	FakeDataChannel.rejectedSubscription = null;
	FakeDataChannel.rejectUnsubscribe = false;
	FakeDataChannel.subscriptionQuality = undefined;
	FakeDataChannel.selectedVariant = null;
	FakePeerConnection.failRemoteDescription = false;
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
