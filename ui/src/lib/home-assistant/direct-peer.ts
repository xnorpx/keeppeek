import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import {
	ControlEnvelopeSchema,
	DeliveryTransport,
	MediaKind,
	RequestSchema,
	SubscribeMediaSchema,
	UnsubscribeSchema,
	VideoQuality,
	type Ok,
	type Request,
	type ServerCapabilities
} from '../proto/webrtc_pb';
import { maxSources, type CardConfig, type CardSource } from './config';
import {
	CardConnectionError,
	createDirectSession,
	deleteDirectSession,
	requestTimeoutMs
} from './direct-http';

type Callbacks = {
	capabilities: (value: ServerCapabilities) => void;
	track: (mid: string) => void;
	failure: (error: CardConnectionError) => void;
};
type Pending = {
	resolve: (value: Ok['result']) => void;
	reject: (error: CardConnectionError) => void;
	timer: ReturnType<typeof setTimeout>;
};

function protocolError(message: string): CardConnectionError {
	return new CardConnectionError('protocol', message);
}

async function withDeadline<Value>(operation: Promise<Value>, message: string): Promise<Value> {
	let timer: ReturnType<typeof setTimeout> | undefined;
	const timeout = new Promise<never>((_resolve, reject) => {
		timer = setTimeout(() => reject(protocolError(message)), requestTimeoutMs);
	});
	try {
		return await Promise.race([operation, timeout]);
	} finally {
		clearTimeout(timer);
	}
}

export class CardSubscriptionError extends CardConnectionError {
	constructor() {
		super('protocol', 'Camera request rejected. Check source availability, codec, and access.');
	}
}

export class DirectPeer {
	#config: Pick<CardConfig, 'endpoint' | 'token'>;
	#callbacks: Callbacks;
	#peer: RTCPeerConnection | null = null;
	#control: RTCDataChannel | null = null;
	#mids = new Map<string, RTCRtpTransceiver>();
	#streams = new Map<string, MediaStream>();
	#subscriptions = new Map<string, string>();
	#pending = new Map<bigint, Pending>();
	#requestId = 1n;
	#closed = false;
	#creation: ReturnType<typeof createDirectSession> | null = null;
	#closing: Promise<void> | null = null;
	#capabilityWaiter: ((value: ServerCapabilities | CardConnectionError) => void) | null = null;

	constructor(config: Pick<CardConfig, 'endpoint' | 'token'>, callbacks: Callbacks) {
		this.#config = config;
		this.#callbacks = callbacks;
	}

	async open(): Promise<ServerCapabilities> {
		if (this.#closed || this.#peer)
			throw protocolError('WebRTC session is closed or already open.');
		const peer = new RTCPeerConnection({ iceServers: [] });
		this.#peer = peer;
		this.createChannels(peer);
		const transceivers = Array.from({ length: maxSources }, () =>
			peer.addTransceiver('video', { direction: 'recvonly' })
		);
		peer.ontrack = (event) => {
			if (!this.#closed && event.transceiver.mid !== null)
				this.#callbacks.track(event.transceiver.mid);
		};
		peer.onconnectionstatechange = () => {
			if (!this.#closed && ['failed', 'disconnected', 'closed'].includes(peer.connectionState)) {
				this.#callbacks.failure(
					new CardConnectionError('network', 'The live connection was interrupted.')
				);
			}
		};
		const offer = await withDeadline(peer.createOffer(), 'WebRTC offer timed out.');
		await withDeadline(peer.setLocalDescription(offer), 'Local WebRTC description timed out.');
		if (this.#closed) throw protocolError('WebRTC session is closed.');
		for (const transceiver of transceivers) {
			if (transceiver.mid === null || this.#mids.has(transceiver.mid))
				throw protocolError('The browser returned an invalid MID.');
			this.#mids.set(transceiver.mid, transceiver);
		}
		if (!peer.localDescription) throw protocolError('The browser did not create a WebRTC offer.');
		this.#creation = createDirectSession(this.#config, peer.localDescription);
		const session = await this.#creation;
		if (this.#closed) throw protocolError('WebRTC session is closed.');
		let timer: ReturnType<typeof setTimeout> | undefined;
		const capabilities = new Promise<ServerCapabilities | CardConnectionError>((resolve) => {
			this.#capabilityWaiter = resolve;
			timer = setTimeout(
				() => resolve(protocolError('KeepPeek capabilities timed out.')),
				requestTimeoutMs
			);
		});
		try {
			await withDeadline(
				peer.setRemoteDescription(session.answer),
				'Remote WebRTC description timed out.'
			);
			const result = await capabilities;
			if (result instanceof CardConnectionError) throw result;
			return result;
		} finally {
			clearTimeout(timer);
			this.#capabilityWaiter = null;
		}
	}

	private createChannels(peer: RTCPeerConnection): void {
		const control = peer.createDataChannel('control-channel', {
			id: 0,
			negotiated: true,
			ordered: true
		});
		const reliable = peer.createDataChannel('reliable-data', {
			id: 1,
			negotiated: true,
			ordered: true
		});
		const unreliable = peer.createDataChannel('unreliable-data', {
			id: 2,
			negotiated: true,
			ordered: false,
			maxRetransmits: 0
		});
		for (const channel of [control, reliable, unreliable]) channel.binaryType = 'arraybuffer';
		this.#control = control;
		control.onmessage = (event) => this.receive(event.data);
		control.onclose = () => {
			if (!this.#closed)
				this.#callbacks.failure(new CardConnectionError('network', 'The control channel closed.'));
		};
		control.onerror = () => {
			if (!this.#closed)
				this.#callbacks.failure(new CardConnectionError('network', 'The control channel failed.'));
		};
	}

	async subscribe(options: {
		sourceSessionId: string;
		subscriptionId: string;
		quality: CardSource['quality'];
	}) {
		const result = await this.request({
			case: 'subscribeMedia',
			value: create(SubscribeMediaSchema, {
				...options,
				kind: MediaKind.VIDEO,
				requestedDeliveryTransport: DeliveryTransport.RTP,
				videoQuality:
					options.quality === 'high'
						? VideoQuality.HIGH
						: options.quality === 'low'
							? VideoQuality.LOW
							: VideoQuality.AUTO
			})
		});
		if (
			result.case !== 'subscriptionResult' ||
			result.value.subscriptionId !== options.subscriptionId ||
			result.value.delivery.case !== 'rtp'
		) {
			throw protocolError('KeepPeek returned an invalid live subscription response.');
		}
		const mid = result.value.delivery.value.mid;
		if (
			!this.#mids.has(mid) ||
			[...this.#subscriptions].some(
				([subscription, boundMid]) => boundMid === mid && subscription !== options.subscriptionId
			)
		) {
			throw protocolError('KeepPeek returned an unknown or occupied MID.');
		}
		this.#subscriptions.set(options.subscriptionId, mid);
		return { mid, variantId: result.value.selectedVariantId };
	}

	async unsubscribe(subscriptionId: string): Promise<void> {
		await this.request({
			case: 'unsubscribe',
			value: create(UnsubscribeSchema, { subscriptionIds: [subscriptionId] })
		});
		this.#subscriptions.delete(subscriptionId);
	}

	stream(mid: string): MediaStream | null {
		const transceiver = this.#mids.get(mid);
		if (!transceiver || this.#closed) return null;
		let stream = this.#streams.get(mid);
		if (!stream) {
			stream = new MediaStream([transceiver.receiver.track]);
			this.#streams.set(mid, stream);
		}
		return stream;
	}

	private request(command: Request['command']): Promise<Ok['result']> {
		const channel = this.#control;
		if (this.#closed || channel?.readyState !== 'open')
			return Promise.reject(protocolError('The live session is closed.'));
		if (this.#pending.size >= 32 || channel.bufferedAmount > 1_048_576)
			return Promise.reject(protocolError('Too many live requests are pending.'));
		const requestId = this.#requestId;
		this.#requestId += 2n;
		return new Promise((resolve, reject) => {
			const timer = setTimeout(() => {
				this.#pending.delete(requestId);
				reject(protocolError('The live request timed out.'));
			}, requestTimeoutMs);
			this.#pending.set(requestId, { resolve, reject, timer });
			try {
				channel.send(
					toBinary(
						ControlEnvelopeSchema,
						create(ControlEnvelopeSchema, {
							message: { case: 'request', value: create(RequestSchema, { requestId, command }) }
						})
					)
				);
			} catch {
				clearTimeout(timer);
				this.#pending.delete(requestId);
				reject(protocolError('The live request could not be sent.'));
			}
		});
	}

	private receive(data: unknown): void {
		if (this.#closed) return;
		if (!(data instanceof ArrayBuffer) || data.byteLength > 1_048_576) {
			this.#callbacks.failure(protocolError('KeepPeek sent an invalid control message.'));
			return;
		}
		try {
			const { message } = fromBinary(ControlEnvelopeSchema, new Uint8Array(data));
			if (message.case === 'notification' && message.value.event.case === 'initialCapabilities') {
				const capabilities = message.value.event.value;
				if (capabilities.cameras.length > 4096 || capabilities.sourceSessions.length > 4096)
					throw protocolError('Too many camera sources.');
				this.#capabilityWaiter?.(capabilities);
				this.#callbacks.capabilities(capabilities);
			} else if (message.case === 'response') {
				const response = message.value;
				const pending = this.#pending.get(response.requestId);
				if (!pending) return;
				this.#pending.delete(response.requestId);
				clearTimeout(pending.timer);
				if (response.result.case === 'ok') pending.resolve(response.result.value.result);
				else pending.reject(new CardSubscriptionError());
			}
		} catch {
			this.#callbacks.failure(protocolError('KeepPeek sent an invalid control message.'));
		}
	}

	close(): Promise<void> {
		if (this.#closing) return this.#closing;
		this.#closed = true;
		const error = protocolError('The live session is closed.');
		this.#capabilityWaiter?.(error);
		for (const pending of this.#pending.values()) {
			clearTimeout(pending.timer);
			pending.reject(error);
		}
		this.#pending.clear();
		for (const transceiver of this.#mids.values()) transceiver.receiver.track.stop();
		this.#peer?.close();
		this.#mids.clear();
		this.#streams.clear();
		this.#subscriptions.clear();
		this.#closing = this.finishClose();
		return this.#closing;
	}

	private async finishClose(): Promise<void> {
		const session = await this.#creation?.catch(() => null);
		if (session) await deleteDirectSession(this.#config, session.session_id);
	}
}
