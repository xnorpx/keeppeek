import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	ControlEnvelopeSchema,
	MediaStreamCapabilitySchema,
	NotificationSchema,
	OkSchema,
	ResponseSchema,
	RtpDeliverySchema,
	ServerCapabilitiesSchema,
	SourceSessionSchema,
	SubscriptionResultSchema
} from './proto/webrtc_pb';

const api = vi.hoisted(() => ({
	createSession: vi.fn(),
	deleteSession: vi.fn()
}));

vi.mock('./api', () => api);

import { LivePeer } from './stream-peer.svelte';

class FakeDataChannel {
	static suppressCapabilities = false;

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
							selectedVariantId: command.value.variantId || 'sub'
						})
					}
				: undefined;
		const response = create(ControlEnvelopeSchema, {
			message: {
				case: 'response',
				value: create(ResponseSchema, {
					requestId: request.requestId,
					result: {
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
	FakePeerConnection.failRemoteDescription = false;
	FakePeerConnection.latest = null;
});

describe('LivePeer', () => {
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
