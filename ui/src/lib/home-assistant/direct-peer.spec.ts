import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	ControlEnvelopeSchema,
	NotificationSchema,
	OkSchema,
	ResponseSchema,
	ServerCapabilitiesSchema,
	type Request
} from '../proto/webrtc_pb';
import { DirectPeer } from './direct-peer';

const http = vi.hoisted(() => ({ createDirectSession: vi.fn(), deleteDirectSession: vi.fn() }));
vi.mock('./direct-http', async (importOriginal) => ({
	...(await importOriginal<typeof import('./direct-http')>()),
	...http
}));

class FakeChannel {
	readyState = 'open';
	binaryType = 'blob';
	onmessage: ((event: MessageEvent) => void) | null = null;
	onclose: (() => void) | null = null;
	onerror: (() => void) | null = null;
	commands: Request[] = [];
	responseMid = 'opaque-7';
	holdResponses = false;

	constructor(
		readonly label: string,
		readonly options: RTCDataChannelInit
	) {}

	send(bytes: Uint8Array): void {
		const envelope = fromBinary(ControlEnvelopeSchema, bytes);
		if (envelope.message.case !== 'request') throw new Error('Expected a request.');
		const request = envelope.message.value;
		this.commands.push(request);
		if (this.holdResponses) return;
		const result =
			request.command.case === 'subscribeMedia'
				? {
						case: 'subscriptionResult' as const,
						value: {
							subscriptionId: request.command.value.subscriptionId,
							selectedVariantId: 'main',
							delivery: { case: 'rtp' as const, value: { mid: this.responseMid } }
						}
					}
				: undefined;
		const response = create(ControlEnvelopeSchema, {
			message: {
				case: 'response',
				value: create(ResponseSchema, {
					requestId: request.requestId,
					result: { case: 'ok', value: create(OkSchema, { result }) }
				})
			}
		});
		queueMicrotask(() => this.deliver(toBinary(ControlEnvelopeSchema, response)));
	}

	deliver(bytes: Uint8Array): void {
		this.onmessage?.(new MessageEvent('message', { data: bytes.slice().buffer }));
	}
}

class FakePeer {
	static instances: FakePeer[] = [];
	static sendCapabilities = true;
	channels: FakeChannel[] = [];
	transceivers: RTCRtpTransceiver[] = [];
	localDescription: RTCSessionDescriptionInit | null = null;
	connectionState = 'new';
	iceGatheringState = 'gathering';
	ontrack: ((event: RTCTrackEvent) => void) | null = null;
	onconnectionstatechange: (() => void) | null = null;
	close = vi.fn(() => {
		this.connectionState = 'closed';
	});

	constructor() {
		FakePeer.instances.push(this);
	}
	createDataChannel(label: string, options: RTCDataChannelInit) {
		const channel = new FakeChannel(label, options);
		this.channels.push(channel);
		return channel;
	}
	addTransceiver(kind: string, options: RTCRtpTransceiverInit) {
		if (kind !== 'video' || options.direction !== 'recvonly') throw new Error('Invalid capacity.');
		const transceiver = {
			mid: `opaque-${this.transceivers.length}`,
			receiver: { track: { kind, stop: vi.fn() } }
		} as unknown as RTCRtpTransceiver;
		this.transceivers.push(transceiver);
		return transceiver;
	}
	async createOffer() {
		if (this.transceivers.length !== 16)
			throw new Error('Capacity was not established before the offer.');
		return { type: 'offer', sdp: 'v=0\r\na=mid:opaque-7' };
	}
	async setLocalDescription(offer: RTCSessionDescriptionInit) {
		this.localDescription = offer;
	}
	async setRemoteDescription() {
		if (!FakePeer.sendCapabilities) return;
		const envelope = create(ControlEnvelopeSchema, {
			message: {
				case: 'notification',
				value: create(NotificationSchema, {
					event: {
						case: 'initialCapabilities',
						value: create(ServerCapabilitiesSchema, { revision: 1n })
					}
				})
			}
		});
		this.channels[0]!.deliver(toBinary(ControlEnvelopeSchema, envelope));
	}
}

function peer() {
	return new DirectPeer(
		{ endpoint: 'https://keeppeek.example.net', token: 'test-credential' },
		{ capabilities: vi.fn(), track: vi.fn(), failure: vi.fn() }
	);
}

beforeEach(() => {
	vi.stubGlobal('RTCPeerConnection', FakePeer);
	vi.stubGlobal(
		'MediaStream',
		class {
			constructor(readonly tracks: MediaStreamTrack[]) {}
		}
	);
	http.createDirectSession.mockResolvedValue({
		session_id: 'session',
		answer: { type: 'answer', sdp: 'v=0' }
	});
	http.deleteDirectSession.mockResolvedValue(undefined);
});

afterEach(() => {
	vi.unstubAllGlobals();
	vi.useRealTimers();
	vi.clearAllMocks();
	vi.restoreAllMocks();
	FakePeer.instances = [];
	FakePeer.sendCapabilities = true;
});

describe('Home Assistant direct WebRTC contract', () => {
	it('bounds a browser offer that never settles', async () => {
		vi.useFakeTimers();
		const offer = vi
			.spyOn(FakePeer.prototype, 'createOffer')
			.mockImplementation(() => new Promise(() => {}));
		const connection = peer();
		const rejected = expect(connection.open()).rejects.toThrow(/offer.*timed out/i);
		await vi.advanceTimersByTimeAsync(10_001);
		await rejected;
		await connection.close();
		expect(http.createDirectSession).not.toHaveBeenCalled();
		offer.mockRestore();
	});

	it('accepts exactly 32 pending requests and rejects the 33rd', async () => {
		const connection = peer();
		await connection.open();
		const channel = FakePeer.instances[0]!.channels[0]!;
		channel.holdResponses = true;
		const pending = Promise.allSettled(
			Array.from({ length: 32 }, () => connection.unsubscribe('pending'))
		);
		await expect(connection.unsubscribe('overflow')).rejects.toThrow('Too many live requests');
		expect(channel.commands).toHaveLength(32);
		await connection.close();
		expect(await pending).toHaveLength(32);
	});

	it('creates the exact channel topology and offers capacity without waiting for ICE', async () => {
		const connection = peer();
		expect((await connection.open()).revision).toBe(1n);
		const browser = FakePeer.instances[0]!;
		expect(browser.channels.map(({ label, options }) => ({ label, options }))).toEqual([
			{ label: 'control-channel', options: { id: 0, negotiated: true, ordered: true } },
			{ label: 'reliable-data', options: { id: 1, negotiated: true, ordered: true } },
			{
				label: 'unreliable-data',
				options: { id: 2, negotiated: true, ordered: false, maxRetransmits: 0 }
			}
		]);
		expect(browser.channels[0]!.commands).toHaveLength(0);
		expect(http.createDirectSession).toHaveBeenCalledWith(
			expect.anything(),
			browser.localDescription
		);
		await connection.close();
	});

	it('maps a subscription to the exact opaque MID returned by the server', async () => {
		const connection = peer();
		await connection.open();
		expect(
			await connection.subscribe({
				subscriptionId: 'card-source',
				sourceSessionId: 'live-source',
				quality: 'auto'
			})
		).toMatchObject({ mid: 'opaque-7', variantId: 'main' });
		expect(connection.stream('opaque-7')).not.toBeNull();
		await connection.unsubscribe('card-source');
		expect(
			FakePeer.instances[0]!.channels[0]!.commands.map((request) => request.command.case)
		).toEqual(['subscribeMedia', 'unsubscribe']);
		await connection.close();
	});

	it('rejects a MID that was not present in the local offer', async () => {
		const connection = peer();
		await connection.open();
		FakePeer.instances[0]!.channels[0]!.responseMid = 'not-offered';
		await expect(
			connection.subscribe({
				subscriptionId: 'card-source',
				sourceSessionId: 'live-source',
				quality: 'low'
			})
		).rejects.toThrow(/MID/);
		await connection.close();
	});

	it('deletes a late-created session exactly once when removed during bootstrap', async () => {
		let finishCreate!: (value: unknown) => void;
		http.createDirectSession.mockImplementation(
			() =>
				new Promise((resolve) => {
					finishCreate = resolve;
				})
		);
		const connection = peer();
		const opening = connection.open();
		const rejected = expect(opening).rejects.toThrow(/closed/i);
		await vi.waitFor(() => expect(http.createDirectSession).toHaveBeenCalledTimes(1));
		const closing = connection.close();
		finishCreate({ session_id: 'late-session', answer: { type: 'answer', sdp: 'v=0' } });
		await closing;
		await rejected;
		await connection.close();
		expect(http.deleteDirectSession).toHaveBeenCalledTimes(1);
		expect(http.deleteDirectSession).toHaveBeenCalledWith(expect.anything(), 'late-session');
	});

	it('bounds the capabilities wait and closes the allocated session', async () => {
		vi.useFakeTimers();
		FakePeer.sendCapabilities = false;
		const connection = peer();
		const opening = connection.open();
		const rejected = expect(opening).rejects.toThrow(/capabilities/i);
		await vi.advanceTimersByTimeAsync(10_001);
		await rejected;
		await connection.close();
		expect(http.deleteDirectSession).toHaveBeenCalledTimes(1);
	});
});
