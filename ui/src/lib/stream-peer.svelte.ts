import { create, fromBinary, toBinary } from '@bufbuild/protobuf';
import { createSession, deleteSession } from './api';
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
	type Response as ControlResponse,
	type ServerCapabilities
} from './proto/webrtc_pb';
import type { LiveQuality } from './types';

const controlTimeoutMs = 10_000;

type PendingRequest = {
	resolve: (response: ControlResponse) => void;
	reject: (error: Error) => void;
	timeout: ReturnType<typeof setTimeout>;
};

export type LivePeerPlan = {
	cameraId: string;
	quality: LiveQuality;
	variantId?: 'main' | 'sub';
};

export type LivePeerTrack = {
	cameraId: string;
	trackId: string;
	receiver: RTCRtpReceiver | null;
	stream: MediaStream | null;
	status: 'connecting' | 'live' | 'unavailable';
	requestedQuality: LiveQuality;
	requestedVariantId: 'main' | 'sub' | null;
	activeStream: 'main' | 'sub';
	pendingStream: 'main' | 'sub' | null;
	estimatedBitrateBps: number | null;
};

export class LivePeer {
	connectionState = $state<RTCPeerConnectionState>('new');
	iceConnectionState = $state<RTCIceConnectionState>('new');
	sessionId = $state<number | null>(null);
	estimatedBitrateBps = $state<number | null>(null);
	error = $state<string | null>(null);
	tracks = $state.raw<Record<string, LivePeerTrack>>({});

	#peer: RTCPeerConnection | null = null;
	#sessionToken: string | null = null;
	#controlChannel: RTCDataChannel | null = null;
	#reliableChannel: RTCDataChannel | null = null;
	#unreliableChannel: RTCDataChannel | null = null;
	#cameraByMid: Record<string, string> = {};
	#sourceSessionByCamera: Record<string, string> = {};
	#trackEventByMid: Record<string, RTCTrackEvent> = {};
	#nextRequestId = 1n;
	#pending = new Map<bigint, PendingRequest>();
	#capabilities: ServerCapabilities | null = null;
	#capabilitiesWaiters: Array<{
		resolve: (capabilities: ServerCapabilities) => void;
		reject: (error: Error) => void;
		timeout: ReturnType<typeof setTimeout>;
	}> = [];
	#viewers: string[] = [];
	#topologyKey = '';
	#operation: Promise<void> = Promise.resolve();
	#closeScheduled = false;
	#holds = 0;

	track(cameraId: string): LivePeerTrack | null {
		return this.tracks[cameraId] ?? null;
	}

	attach(cameraId: string): () => void {
		this.#viewers.push(cameraId);
		return () => this.detach(cameraId);
	}

	hold(): () => void {
		this.#holds += 1;
		let held = true;
		return () => {
			if (!held) return;
			held = false;
			this.#holds -= 1;
			if (this.#viewers.length === 0 && this.#holds === 0) this.scheduleClose();
		};
	}

	configure(plans: LivePeerPlan[]): Promise<void> {
		const nextPlans = plans
			.map((plan) => ({ ...plan }))
			.toSorted((left, right) => left.cameraId.localeCompare(right.cameraId));
		const cameraIds = Object.create(null) as Record<string, true>;
		for (const plan of nextPlans) {
			if (cameraIds[plan.cameraId]) {
				return Promise.reject(new Error('Live peer plans must contain each camera at most once'));
			}
			cameraIds[plan.cameraId] = true;
		}
		return this.enqueue(async () => this.configureNow(nextPlans));
	}

	close(): Promise<void> {
		return this.enqueue(async () => this.closeNow());
	}

	closeOnPageHide(): void {
		const sessionToken = this.releaseLocalResources();
		if (sessionToken === null) return;
		const body = new Blob([JSON.stringify({ session_id: sessionToken })], {
			type: 'application/json'
		});
		navigator.sendBeacon('/delete', body);
	}

	markPlaying(cameraId: string): void {
		const track = this.track(cameraId);
		if (!track) return;
		this.replaceTrack(cameraId, { status: 'live' });
	}

	markUnavailable(cameraId: string): void {
		const track = this.track(cameraId);
		if (!track) return;
		this.replaceTrack(cameraId, { status: 'unavailable' });
	}

	private enqueue(task: () => Promise<void>): Promise<void> {
		this.#operation = this.#operation.catch(() => undefined).then(task);
		return this.#operation;
	}

	private async configureNow(plans: LivePeerPlan[]): Promise<void> {
		const topologyKey = plans.map((plan) => plan.cameraId).join('\u0000');
		if (this.#peer === null || this.#topologyKey !== topologyKey) {
			await this.closeNow();
			if (plans.length === 0) return;
			await this.connect(plans, topologyKey);
			return;
		}
		await Promise.all(
			plans
				.filter((plan) => {
					const track = this.track(plan.cameraId);
					return (
						track?.requestedQuality !== plan.quality ||
						track?.requestedVariantId !== (plan.variantId ?? null)
					);
				})
				.map((plan) => this.setQuality(plan.cameraId, plan.quality, plan.variantId ?? null))
		);
	}

	private async connect(plans: LivePeerPlan[], topologyKey: string): Promise<void> {
		const peer = new RTCPeerConnection();
		const controlChannel = peer.createDataChannel('control-channel', {
			negotiated: true,
			id: 0,
			ordered: true
		});
		const reliableChannel = peer.createDataChannel('reliable-data', {
			negotiated: true,
			id: 1,
			ordered: true
		});
		const unreliableChannel = peer.createDataChannel('unreliable-data', {
			negotiated: true,
			id: 2,
			ordered: false,
			maxRetransmits: 0
		});
		controlChannel.binaryType = 'arraybuffer';
		reliableChannel.binaryType = 'arraybuffer';
		unreliableChannel.binaryType = 'arraybuffer';
		this.#peer = peer;
		this.#controlChannel = controlChannel;
		this.#reliableChannel = reliableChannel;
		this.#unreliableChannel = unreliableChannel;
		this.#topologyKey = topologyKey;
		this.connectionState = peer.connectionState;
		this.iceConnectionState = peer.iceConnectionState;
		this.error = null;
		this.#cameraByMid = {};
		this.#sourceSessionByCamera = {};
		this.#trackEventByMid = {};
		this.#capabilities = null;
		const controlOpened = waitForDataChannel(controlChannel);
		controlChannel.onmessage = (event) => this.receiveControl(event);
		controlChannel.onclose = () => this.failPending('WebRTC control channel closed.');

		const localTracks = plans.map((plan, index) => {
			const trackId = `camera-${index}`;
			const transceiver = peer.addTransceiver('video', { direction: 'recvonly' });
			preferH265(transceiver);
			return { ...plan, trackId, transceiver };
		});

		this.tracks = Object.fromEntries(
			localTracks.map((track) => [
				track.cameraId,
				{
					cameraId: track.cameraId,
					trackId: track.trackId,
					receiver: null,
					stream: null,
					status: 'connecting',
					requestedQuality: track.quality,
					requestedVariantId: track.variantId ?? null,
					activeStream: 'sub',
					pendingStream: null,
					estimatedBitrateBps: null
				} satisfies LivePeerTrack
			])
		);

		peer.ontrack = (event) => {
			if (peer !== this.#peer || event.transceiver.mid === null) return;
			this.#trackEventByMid[event.transceiver.mid] = event;
			const cameraId = this.#cameraByMid[event.transceiver.mid];
			if (!cameraId) return;
			this.attachTrackEvent(cameraId, event);
		};
		peer.onconnectionstatechange = () => {
			if (peer !== this.#peer) return;
			this.connectionState = peer.connectionState;
			if (['failed', 'disconnected', 'closed'].includes(peer.connectionState)) {
				this.markAllUnavailable();
				void this.close();
			}
		};
		peer.oniceconnectionstatechange = () => {
			if (peer !== this.#peer) return;
			this.iceConnectionState = peer.iceConnectionState;
			if (peer.iceConnectionState === 'disconnected') {
				this.markAllUnavailable();
				void this.close();
			}
		};

		try {
			const offer = await peer.createOffer();
			await peer.setLocalDescription(offer);
			await waitForIceGathering(peer);

			if (peer !== this.#peer || !peer.localDescription) return;

			for (const track of localTracks) {
				if (track.transceiver.mid === null) {
					throw new Error(`No SDP MID assigned for ${track.cameraId}`);
				}
			}

			const session = await createSession(peer.localDescription);
			if (peer !== this.#peer) {
				await deleteSession(session.session_id);
				return;
			}

			this.#sessionToken = session.session_id;
			const numericSessionId = Number(session.session_id);
			this.sessionId = Number.isSafeInteger(numericSessionId) ? numericSessionId : null;
			await peer.setRemoteDescription({
				type: session.answer.type as RTCSdpType,
				sdp: session.answer.sdp
			});
			if (controlChannel.readyState !== 'open') await controlOpened;
			await this.waitForCapabilities();
			for (const track of localTracks) {
				await this.subscribeTrack(
					track.cameraId,
					track.trackId,
					track.quality,
					track.variantId ?? null
				);
			}
		} catch (error) {
			if (peer === this.#peer) {
				const sessionToken = this.releaseLocalResources();
				this.error = error instanceof Error ? error.message : 'Unable to start shared live view';
				if (sessionToken !== null) {
					try {
						await deleteSession(sessionToken);
					} catch (closeError) {
						console.debug('Unable to close failed shared live session', closeError);
					}
				}
			}
			throw error;
		}
	}
	private async setQuality(
		cameraId: string,
		quality: LiveQuality,
		variantId: 'main' | 'sub' | null
	): Promise<void> {
		const track = this.tracks[cameraId];
		if (!track) return;
		await this.subscribeTrack(cameraId, track.trackId, quality, variantId, true);
	}

	private async subscribeTrack(
		cameraId: string,
		subscriptionId: string,
		quality: LiveQuality,
		variantId: 'main' | 'sub' | null = null,
		replacement = false
	): Promise<void> {
		const sourceSessionId = this.#sourceSessionByCamera[cameraId];
		if (!sourceSessionId) throw new Error(`Camera ${cameraId} has no live source session.`);
		const subscribe = create(SubscribeMediaSchema, {
			subscriptionId,
			sourceSessionId,
			kind: MediaKind.VIDEO,
			requestedDeliveryTransport: DeliveryTransport.RTP,
			videoQuality: protoQuality(quality),
			variantId: variantId ?? ''
		});
		const result = await this.request({ case: 'subscribeMedia', value: subscribe });
		if (result.case !== 'subscriptionResult' || result.value.delivery.case !== 'rtp') {
			throw new Error('Server returned an unexpected media subscription response.');
		}
		const mid = result.value.delivery.value.mid;
		const selectedStream = result.value.selectedVariantId === 'main' ? 'main' : 'sub';
		const current = this.track(cameraId);
		const pendingStream =
			replacement && current !== null && current.activeStream !== selectedStream
				? selectedStream
				: null;
		this.#cameraByMid[mid] = cameraId;
		this.replaceTrack(cameraId, {
			requestedQuality: quality,
			requestedVariantId: variantId,
			activeStream: pendingStream === null ? selectedStream : current!.activeStream,
			pendingStream
		});
		const event = this.#trackEventByMid[mid];
		if (event) this.attachTrackEvent(cameraId, event);
	}

	private attachTrackEvent(cameraId: string, event: RTCTrackEvent): void {
		const mediaTrack = event.track ?? event.receiver.track;
		const stream = mediaTrack
			? new MediaStream([mediaTrack])
			: (event.streams[0] ?? new MediaStream());
		this.replaceTrack(cameraId, { receiver: event.receiver, stream, status: 'live' });
	}

	private async request(command: Request['command']): Promise<Ok['result']> {
		const channel = this.#controlChannel;
		if (!channel || channel.readyState !== 'open') {
			throw new Error('WebRTC control channel is unavailable.');
		}
		const requestId = this.#nextRequestId;
		this.#nextRequestId += 2n;
		const response = new Promise<ControlResponse>((resolve, reject) => {
			const timeout = setTimeout(() => {
				this.#pending.delete(requestId);
				reject(new Error('WebRTC media request timed out.'));
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
		if (reply.result.case !== 'ok') throw new Error('Server returned an empty media response.');
		return reply.result.value.result;
	}

	private receiveControl(event: MessageEvent): void {
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
			const event = envelope.message.value.event;
			if (event.case === 'initialCapabilities') {
				const capabilities = event.value;
				this.#capabilities = capabilities;
				this.#sourceSessionByCamera = Object.fromEntries(
					capabilities.sourceSessions
						.filter((source) => source.sourceId.length > 0 && source.video !== undefined)
						.map((source) => [source.sourceId, source.sourceSessionId])
				);
				for (const waiter of this.#capabilitiesWaiters) {
					clearTimeout(waiter.timeout);
					waiter.resolve(capabilities);
				}
				this.#capabilitiesWaiters = [];
			}
			if (event.case === 'subscriptionStreamState') {
				const stream = event.value.activeVariantId;
				if (stream !== 'main' && stream !== 'sub') return;
				const cameraId = Object.values(this.tracks).find(
					(track) => track.trackId === event.value.subscriptionId
				)?.cameraId;
				if (cameraId !== undefined) {
					this.replaceTrack(cameraId, { activeStream: stream, pendingStream: null });
				}
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

	private waitForCapabilities(): Promise<ServerCapabilities> {
		if (this.#capabilities) return Promise.resolve(this.#capabilities);
		return new Promise((resolve, reject) => {
			const timeout = setTimeout(() => {
				this.#capabilitiesWaiters = this.#capabilitiesWaiters.filter(
					(waiter) => waiter.resolve !== resolve
				);
				reject(new Error('WebRTC server capabilities did not arrive.'));
			}, controlTimeoutMs);
			this.#capabilitiesWaiters.push({ resolve, reject, timeout });
		});
	}

	private failPending(message: string): void {
		for (const pending of this.#pending.values()) {
			clearTimeout(pending.timeout);
			pending.reject(new Error(message));
		}
		this.#pending.clear();
		for (const waiter of this.#capabilitiesWaiters) {
			clearTimeout(waiter.timeout);
			waiter.reject(new Error(message));
		}
		this.#capabilitiesWaiters = [];
	}

	private replaceTrack(cameraId: string, update: Partial<LivePeerTrack>): void {
		const current = this.track(cameraId);
		if (!current) return;
		this.tracks = { ...this.tracks, [cameraId]: { ...current, ...update } };
	}

	private async closeNow(): Promise<void> {
		if (this.#controlChannel?.readyState === 'open' && Object.keys(this.tracks).length > 0) {
			try {
				await this.request({
					case: 'unsubscribe',
					value: create(UnsubscribeSchema, {
						subscriptionIds: Object.values(this.tracks).map((track) => track.trackId)
					})
				});
			} catch {
				// Session deletion remains authoritative teardown.
			}
		}
		const sessionToken = this.releaseLocalResources();
		if (sessionToken === null) return;
		try {
			await deleteSession(sessionToken);
		} catch (error) {
			console.debug('Unable to close shared live session', error);
		}
	}

	private releaseLocalResources(): string | null {
		const peer = this.#peer;
		const sessionToken = this.#sessionToken;
		this.#peer = null;
		this.#sessionToken = null;
		this.#topologyKey = '';
		this.#cameraByMid = {};
		this.#sourceSessionByCamera = {};
		this.#trackEventByMid = {};
		this.#capabilities = null;
		this.sessionId = null;
		this.estimatedBitrateBps = null;
		for (const stream of Object.values(this.tracks).flatMap((track) =>
			track.stream ? [track.stream] : []
		)) {
			for (const track of stream.getTracks()) track.stop();
		}
		if (peer) {
			peer.ontrack = null;
			peer.onconnectionstatechange = null;
			peer.oniceconnectionstatechange = null;
		}
		this.#controlChannel?.close();
		this.#reliableChannel?.close();
		this.#unreliableChannel?.close();
		this.#controlChannel = null;
		this.#reliableChannel = null;
		this.#unreliableChannel = null;
		peer?.close();
		this.failPending('WebRTC media session closed.');
		this.tracks = {};
		this.connectionState = peer ? 'closed' : 'new';
		this.iceConnectionState = 'closed';
		return sessionToken;
	}

	private detach(cameraId: string): void {
		const index = this.#viewers.indexOf(cameraId);
		if (index < 0) return;
		this.#viewers.splice(index, 1);
		if (this.#viewers.length === 0 && this.#holds === 0) this.scheduleClose();
	}

	private scheduleClose(): void {
		if (this.#closeScheduled) return;
		this.#closeScheduled = true;
		queueMicrotask(() => {
			this.#closeScheduled = false;
			if (this.#viewers.length === 0 && this.#holds === 0) void this.close();
		});
	}

	private markAllUnavailable(): void {
		this.tracks = Object.fromEntries(
			Object.entries(this.tracks).map(([cameraId, track]) => [
				cameraId,
				{ ...track, status: 'unavailable' }
			])
		);
	}
}

function preferH265(transceiver: RTCRtpTransceiver): void {
	const codecs = RTCRtpReceiver.getCapabilities?.('video')?.codecs;
	if (!codecs || !transceiver.setCodecPreferences) return;
	const h265 = codecs.filter((codec) => codec.mimeType.toLowerCase() === 'video/h265');
	if (h265.length === 0) return;
	transceiver.setCodecPreferences([
		...h265,
		...codecs.filter((codec) => codec.mimeType.toLowerCase() !== 'video/h265')
	]);
}

function protoQuality(quality: LiveQuality): VideoQuality {
	if (quality === 'high') return VideoQuality.HIGH;
	if (quality === 'low') return VideoQuality.LOW;
	return VideoQuality.AUTO;
}

function waitForDataChannel(channel: RTCDataChannel): Promise<void> {
	if (channel.readyState === 'open') return Promise.resolve();
	return new Promise((resolve, reject) => {
		const timeout = setTimeout(
			() => reject(new Error(`WebRTC ${channel.label} channel did not open.`)),
			controlTimeoutMs
		);
		channel.onopen = () => {
			clearTimeout(timeout);
			resolve();
		};
		channel.onerror = () => {
			clearTimeout(timeout);
			reject(new Error(`WebRTC ${channel.label} channel failed.`));
		};
	});
}

function waitForIceGathering(peer: RTCPeerConnection): Promise<void> {
	if (peer.iceGatheringState === 'complete') return Promise.resolve();
	return new Promise((resolve) => {
		const changed = () => {
			if (peer.iceGatheringState === 'complete') {
				peer.removeEventListener('icegatheringstatechange', changed);
				resolve();
			}
		};
		peer.addEventListener('icegatheringstatechange', changed);
		changed();
	});
}
