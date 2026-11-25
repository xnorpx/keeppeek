import { create, equals, fromBinary, toBinary } from '@bufbuild/protobuf';
import { ApiRequestError, createSession, deleteSession } from './api';
import { AacAudioPlayback } from './audio-playback';
import { replaceKnownCamera } from './camera-inventory';
import { liveVariant, safeBitrate } from './live-quality';
import {
	ControlEnvelopeSchema,
	CameraControlCommandSchema,
	AccessRole,
	DeliveryTransport,
	MediaKind,
	LiveQualitySelectionReason,
	MessageSchema,
	RequestSchema,
	SubscribeMediaSchema,
	TalkbackCommandSchema,
	StartTalkbackSchema,
	PlayQuickReplySchema,
	PlayChimeSchema,
	StopTalkbackSchema,
	TalkbackTargetSchema,
	AllTalkbackTargetsSchema,
	UnsubscribeSchema,
	VideoQuality,
	type Ok,
	type Request,
	type SubscribeMedia,
	type Response as ControlResponse,
	type CameraInfo,
	type CameraUpdated,
	type ServerCapabilities,
	type LiveQualityDiagnostics,
	type LiveAudioTiming,
	type MediaVariantCapability,
	type SubscriptionResult,
	type SubscriptionStreamState,
	type TalkbackState,
	type TalkbackTarget
} from './proto/webrtc_pb';
import type { CreateResponse, LiveQuality } from './types';

const controlTimeoutMs = 10_000;

class MediaAdmissionError extends Error {}

export type LiveSessionTransport = {
	create: (offer: RTCSessionDescriptionInit, previousSession?: string) => Promise<CreateResponse>;
	delete: (sessionId: string, options?: { keepalive?: boolean }) => Promise<void>;
};

const defaultSessionTransport: LiveSessionTransport = {
	create: (offer, previousSession) => createSession(offer, null, previousSession),
	delete: (sessionId, options) => deleteSession(sessionId, null, options)
};

type NegotiatedTrack = LivePeerPlan & {
	plan: LivePeerPlan;
	trackId: string;
	audioTrackId: string;
	transceiver: RTCRtpTransceiver;
	audioTransceiver: RTCRtpTransceiver;
};

type AudioDelivery = 'rtp' | 'mediaData';

type PendingRequest = {
	resolve: (response: ControlResponse) => void;
	reject: (error: Error) => void;
	timeout: ReturnType<typeof setTimeout>;
};

class SupersededPreview extends Error {
	constructor() {
		super('The live timing preview was superseded.');
	}
}

export type LivePeerPlan = {
	cameraId: string;
	quality: LiveQuality;
	variantId?: 'main' | 'sub';
	liveAudioPreviewOffsetMs?: number;
	active?: boolean;
};

export type LivePeerTrack = {
	cameraId: string;
	trackId: string;
	audioTrackId: string;
	receiver: RTCRtpReceiver | null;
	stream: MediaStream | null;
	audioReceiver: RTCRtpReceiver | null;
	audioStream: MediaStream | null;
	status: 'queued' | 'connecting' | 'live' | 'unavailable';
	requestedQuality: LiveQuality;
	requestedVariantId: 'main' | 'sub' | null;
	activeStream: 'main' | 'sub';
	confirmedVideoVariant?: 'main' | 'sub';
	pendingStream: 'main' | 'sub' | null;
	estimatedBitrateBps: number | null;
	liveQuality?: LiveQualityDiagnostics;
	liveAudio?: LiveAudioTiming;
	audioVariantId?: string;
	audioDelivery?: SubscriptionResult['delivery']['case'];
	audioTiming?: LiveAudioTiming;
	timingReconciliationNeeded?: boolean;
	restoredSavedTiming?: boolean;
	liveAudioPreviewOffsetMs?: number;
	subscribed: boolean;
	audioAdmission?: 'retry' | 'admitted';
	audioSourceSessionId?: string;
	admissionError?: string | null;
};

export type TalkbackTargetSelection = { sourceId: string } | { groupId: string } | { all: true };

export type CameraAudioAssets = {
	twoWayAudio: boolean;
	quickReplies: ReadonlyArray<{ id: string; label: string }>;
	chimes: ReadonlyArray<{ id: string; label: string }>;
};

function audioAssets(camera: CameraInfo): CameraAudioAssets | undefined {
	const device = camera.deviceCapabilities;
	if (!device || (!device.twoWayAudio && !device.quickReplies.length && !device.chimes.length))
		return undefined;
	return {
		twoWayAudio: device.twoWayAudio,
		quickReplies: device.quickReplies.map((asset) => ({
			id: asset.assetId,
			label: asset.displayName
		})),
		chimes: device.chimes.map((asset) => ({ id: asset.assetId, label: asset.displayName }))
	};
}

export class LivePeer {
	connectionState = $state<RTCPeerConnectionState>('new');
	iceConnectionState = $state<RTCIceConnectionState>('new');
	sessionId = $state<string | null>(null);
	estimatedBitrateBps = $state<number | null>(null);
	error = $state<string | null>(null);
	talkbackActive = $state(false);
	talkbackError = $state<string | null>(null);
	tracks = $state.raw<Record<string, LivePeerTrack>>({});
	talkbackGroups = $state.raw<ReadonlyArray<{ id: string; label: string }>>([]);
	cameraAudioAssets = $state.raw<Record<string, CameraAudioAssets>>({});

	#peer: RTCPeerConnection | null = null;
	#closingPeer: RTCPeerConnection | null = null;
	#sessionToken: string | null = null;
	#previousSession: string | undefined;
	#controlChannel: RTCDataChannel | null = null;
	#reliableChannel: RTCDataChannel | null = null;
	#unreliableChannel: RTCDataChannel | null = null;
	#talkbackTransceiver: RTCRtpTransceiver | null = null;
	#peerLifetime = new AbortController();
	#talkbackAuthorization = $state.raw<symbol | null>(null);
	#microphone: {
		stream: MediaStream;
		sender: RTCRtpSender;
		target: TalkbackTarget;
		requested: boolean;
	} | null = null;
	#cameraByMid: Record<string, string> = {};
	#audioCameraByMid: Record<string, string> = {};
	#audioContext: AudioContext | null = null;
	#audioPlaybackByBinding = new Map<string, AacAudioPlayback>();
	#audioBindingByCamera: Record<string, string> = {};
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
	#desiredPlans: LivePeerPlan[] = [];
	#recoveryTimer?: ReturnType<typeof setTimeout>;
	#recoveryAttempts = 0;
	#recoveryStopped = false;
	#healthySince?: { peer: RTCPeerConnection; at: number };
	#operation: Promise<void> = Promise.resolve();
	#closeScheduled = false;
	#holds = 0;
	#previewWaiters = new Map<string, (failure?: Error) => void>();
	#audioSync = new Map<
		string,
		{
			variant: string;
			delivery: AudioDelivery;
			attempts: number;
			busy: boolean;
			retry?: ReturnType<typeof setTimeout>;
		}
	>();
	#sessionTransport: LiveSessionTransport;

	constructor(sessionTransport: LiveSessionTransport = defaultSessionTransport) {
		this.#sessionTransport = sessionTransport;
	}

	track(cameraId: string): LivePeerTrack | null {
		return this.tracks[cameraId] ?? null;
	}

	needsAdmission(cameraId: string): boolean {
		const track = this.track(cameraId);
		return !track?.subscribed || track.audioAdmission === 'retry';
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

	configure(plans: LivePeerPlan[], intent: 'open' | 'reconcile' = 'open'): Promise<void> {
		const nextPlans = plans
			.map((plan) => ({ ...plan, active: plan.active ?? true }))
			.toSorted((left, right) => left.cameraId.localeCompare(right.cameraId));
		const cameraIds = Object.create(null) as Record<string, true>;
		for (const plan of nextPlans) {
			if (cameraIds[plan.cameraId]) {
				return Promise.reject(new Error('Live peer plans must contain each camera at most once'));
			}
			cameraIds[plan.cameraId] = true;
		}
		const wasActive = this.#desiredPlans.some((plan) => plan.active);
		this.setDesiredPlans(nextPlans);
		return this.enqueue(async () => {
			if (this.#desiredPlans !== nextPlans) return;
			const recovering = this.#recoveryAttempts > 0 || this.#recoveryStopped;
			if (recovering && !this.#sessionToken) {
				if (intent === 'reconcile') {
					if (!wasActive && nextPlans.some((plan) => plan.active)) this.scheduleRecovery();
					return;
				}
				this.resetRecoveryBudget();
			}
			try {
				await this.configureNow(nextPlans);
				if (!nextPlans.some((plan) => plan.active)) this.cancelRecovery();
				else this.markTransportHealthy();
			} catch (error) {
				if (this.isAccessDenied(error)) this.#recoveryStopped = true;
				else if (recovering || intent === 'reconcile') this.scheduleRecovery();
				throw error;
			}
		});
	}

	private setDesiredPlans(plans: LivePeerPlan[]): void {
		this.#desiredPlans = plans;
		for (const observe of this.#previewWaiters.values()) observe();
	}

	close(): Promise<void> {
		this.talkbackError = null;
		this.setDesiredPlans([]);
		this.cancelRecovery();
		return this.enqueue(async () => this.closeNow());
	}

	get canUseMicrophone(): boolean {
		return this.#talkbackAuthorization !== null;
	}

	get canBroadcastTalkback(): boolean {
		return (
			this.canUseMicrophone &&
			Object.values(this.cameraAudioAssets).some((camera) => camera.twoWayAudio)
		);
	}

	private hasTalkbackTarget(target: TalkbackTargetSelection): boolean {
		if ('sourceId' in target) return this.cameraAudioAssets[target.sourceId]?.twoWayAudio === true;
		if ('groupId' in target)
			return this.talkbackGroups.some((group) => group.id === target.groupId);
		return this.canBroadcastTalkback;
	}

	startTalkback(target: TalkbackTargetSelection): Promise<void> {
		return this.enqueueCommand(async () => {
			const transceiver = this.#talkbackTransceiver;
			const signal = this.#peerLifetime.signal;
			if (transceiver === null) {
				throw new Error('Talkback is unavailable until the live session is connected.');
			}
			const authorization = this.#talkbackAuthorization;
			if (authorization === null)
				throw new Error('Administrator access is required to use the microphone.');
			if (!this.hasTalkbackTarget(target))
				throw new Error('No available speaker matches the selected talkback target.');
			if (this.talkbackActive) return;
			const stream = await acquireTalkbackMicrophone(signal);
			const track = stream.getAudioTracks()[0];
			if (
				!track ||
				this.#talkbackTransceiver !== transceiver ||
				this.#talkbackAuthorization !== authorization ||
				!this.hasTalkbackTarget(target)
			) {
				stream.getTracks().forEach((item) => item.stop());
				throw new Error(
					track
						? 'The live session changed before talkback started.'
						: 'The microphone did not provide an audio track.'
				);
			}
			const microphone = {
				stream,
				sender: transceiver.sender,
				target: talkbackTarget(target),
				requested: false
			};
			this.#microphone = microphone;
			try {
				await microphone.sender.replaceTrack(track);
				signal.throwIfAborted();
				if (this.#microphone !== microphone)
					throw new Error('The live session changed before talkback started.');
				microphone.requested = true;
				await this.request({
					case: 'talkbackCommand',
					value: create(TalkbackCommandSchema, {
						action: {
							case: 'start',
							value: create(StartTalkbackSchema, { target: microphone.target })
						}
					})
				});
				signal.throwIfAborted();
				if (this.#microphone !== microphone)
					throw new Error('The talkback route ended before the microphone started.');
				this.talkbackActive = true;
			} catch (error) {
				if (this.#microphone === microphone) {
					this.releaseMicrophone();
					this.talkbackError = error instanceof Error ? error.message : 'Talkback could not start.';
				}
				throw error;
			}
		});
	}

	stopTalkback(): Promise<void> {
		return this.enqueueCommand(async () => {
			if (!this.talkbackActive) return;
			this.releaseMicrophone();
			await this.request({
				case: 'talkbackCommand',
				value: create(TalkbackCommandSchema, {
					action: { case: 'stop', value: create(StopTalkbackSchema) }
				})
			});
		});
	}

	private receiveTalkbackState(state: TalkbackState): void {
		const microphone = this.#microphone;
		if (
			!microphone?.requested ||
			state.ownerSessionId !== this.sessionId ||
			!state.target ||
			!equals(TalkbackTargetSchema, state.target, microphone.target) ||
			state.activeSourceIds.length !== 0 ||
			state.failedSourceIds.length === 0
		)
			return;
		const selection = microphone.target.selection;
		if (selection.case === 'sourceId' && !state.failedSourceIds.includes(selection.value)) return;
		this.releaseMicrophone();
		this.talkbackError = 'All selected talkback routes ended. The microphone was released.';
	}

	private releaseMicrophone(): void {
		const microphone = this.#microphone;
		if (!microphone) return;
		this.#microphone = null;
		this.talkbackActive = false;
		for (const track of microphone.stream.getTracks()) track.stop();
		// The microphone is already stopped even if a closing peer rejects the detach.
		void microphone.sender.replaceTrack(null).catch(() => undefined);
	}

	playQuickReply(sourceId: string, assetId: string): Promise<void> {
		return this.enqueueCommand(async () => {
			await this.request({
				case: 'cameraControlCommand',
				value: create(CameraControlCommandSchema, {
					action: {
						case: 'playQuickReply',
						value: create(PlayQuickReplySchema, { sourceId, assetId })
					}
				})
			});
		});
	}

	playChime(sourceId: string, assetId: string): Promise<void> {
		return this.enqueueCommand(async () => {
			await this.request({
				case: 'cameraControlCommand',
				value: create(CameraControlCommandSchema, {
					action: {
						case: 'playChime',
						value: create(PlayChimeSchema, { sourceId, assetId })
					}
				})
			});
		});
	}

	resumeAudioPlayback(): Promise<void> {
		return this.#audioContext?.state === 'suspended'
			? this.#audioContext.resume()
			: Promise.resolve();
	}

	closeOnPageHide(): void {
		this.talkbackError = null;
		this.setDesiredPlans([]);
		this.cancelRecovery();
		const sessionToken = this.releaseLocalResources();
		if (sessionToken === null) return;
		void this.#sessionTransport.delete(sessionToken, { keepalive: true });
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

	private enqueueCommand(task: () => Promise<void>): Promise<void> {
		const owner = this.#peer;
		return this.enqueue(async () => {
			try {
				if (!owner) throw new Error('Connect to live view before sending this command.');
				if (owner !== this.#peer)
					throw new Error('The live connection changed. Send the command again if needed.');
				this.talkbackError = null;
				await task();
			} catch (error) {
				const failure =
					owner && owner !== this.#peer
						? new Error(
								'The live connection changed before confirming the command. Check whether it ran before trying again.'
							)
						: error;
				this.talkbackError =
					failure instanceof Error ? failure.message : 'The live command failed.';
				throw failure;
			}
		});
	}

	private isAccessDenied(error: unknown): boolean {
		return error instanceof ApiRequestError && [401, 403].includes(error.status);
	}

	resumeRecovery(): void {
		if (!this.#recoveryStopped) return;
		this.resetRecoveryBudget();
		this.scheduleRecovery();
	}

	private resetRecoveryBudget(): void {
		if (this.#recoveryTimer !== undefined) clearTimeout(this.#recoveryTimer);
		this.#recoveryTimer = undefined;
		this.#recoveryAttempts = 0;
		this.#recoveryStopped = false;
	}

	private cancelRecovery(): void {
		this.resetRecoveryBudget();
		if (this.#previousSession) {
			void this.#sessionTransport.delete(this.#previousSession).catch(() => undefined);
			this.#previousSession = undefined;
		}
		this.#healthySince = undefined;
	}

	private markTransportHealthy(): void {
		if (this.#peer && this.#healthySince?.peer !== this.#peer)
			this.#healthySince = { peer: this.#peer, at: performance.now() };
	}

	private transportLost(owner: RTCPeerConnection): void {
		if (owner !== this.#peer) return;
		if (owner === this.#closingPeer) {
			this.releaseLocalResources();
			return;
		}
		// A replacement must survive a full wall health refresh before it earns a
		// fresh retry budget. Rapid successful handshakes still count as failures.
		if (this.#healthySince?.peer === owner && performance.now() - this.#healthySince.at >= 5000)
			this.#recoveryAttempts = 0;
		this.#healthySince = undefined;
		const token = this.releaseLocalResources();
		if (token) this.#previousSession = token;
		this.scheduleRecovery();
	}

	private scheduleRecovery(): void {
		if (
			this.#recoveryStopped ||
			this.#recoveryTimer !== undefined ||
			!this.#desiredPlans.some((plan) => plan.active) ||
			this.#peer
		)
			return;
		const delays = [0, 750, 2000];
		if (this.#recoveryAttempts >= delays.length) {
			this.#recoveryStopped = true;
			this.error = 'Unable to reconnect live media. Reopen the viewer to try again.';
			return;
		}
		const delay = delays[this.#recoveryAttempts++];
		this.#recoveryTimer = setTimeout(() => {
			this.#recoveryTimer = undefined;
			void this.enqueue(async () => {
				if (this.#peer || !this.#desiredPlans.some((plan) => plan.active)) return;
				await this.configureNow(this.#desiredPlans, true);
				this.markTransportHealthy();
			}).catch((error) => {
				if (this.isAccessDenied(error)) {
					this.#recoveryStopped = true;
					return;
				}
				this.scheduleRecovery();
			});
		}, delay);
	}

	private enqueue(task: () => Promise<void>): Promise<void> {
		this.#operation = this.#operation.catch(() => undefined).then(task);
		return this.#operation;
	}

	private async configureNow(plans: LivePeerPlan[], recovering = false): Promise<void> {
		const topologyKey = plans.map((plan) => plan.cameraId).join('\u0000');
		if (plans.length === 0 && this.#peer === null) return;
		if (this.#peer === null || this.#topologyKey !== topologyKey) {
			await this.closeNow();
			if (plans.length === 0) return;
			await this.connect(plans, topologyKey, recovering);
			return;
		}
		await this.releaseInactiveTracks(plans);
		await Promise.all(
			plans.map(async (plan) => {
				const track = this.track(plan.cameraId);
				if (!track) return;
				if (!plan.active) return;
				const variant = plan.variantId ?? null;
				const timingChanged =
					track.timingReconciliationNeeded ||
					track.liveAudioPreviewOffsetMs !== plan.liveAudioPreviewOffsetMs ||
					(plan.liveAudioPreviewOffsetMs !== undefined && track.requestedVariantId !== variant);
				if (
					track.subscribed &&
					!timingChanged &&
					track.requestedQuality === plan.quality &&
					track.requestedVariantId === variant
				) {
					if (track.audioAdmission === 'retry' && plan.liveAudioPreviewOffsetMs === undefined)
						this.followActiveAudio(plan.cameraId, true);
					return;
				}
				try {
					await this.subscribeTrack(
						plan,
						track.trackId,
						plan.quality,
						variant,
						track.subscribed,
						plan.liveAudioPreviewOffsetMs,
						timingChanged
					);
				} catch (error) {
					if (error instanceof SupersededPreview) {
						this.replaceTrack(plan.cameraId, { timingReconciliationNeeded: true });
					} else if (track.subscribed && timingChanged) {
						await this.restoreTimingPair(plan, track);
					} else if (timingChanged && this.#peer) {
						await this.unsubscribeTrack(plan.cameraId, track.trackId).catch((cleanupError) =>
							console.warn('Unable to release an unconfirmed preview', cleanupError)
						);
					}
					throw error;
				}
			})
		);
	}

	private async restoreTimingPair(plan: LivePeerPlan, track: LivePeerTrack) {
		await this.subscribeTrack(
			plan,
			track.trackId,
			track.requestedQuality,
			track.requestedVariantId,
			true,
			track.liveAudioPreviewOffsetMs,
			true
		)
			.then(() => {
				if (!this.#desiredPlans.includes(plan)) return;
				this.#desiredPlans = this.#desiredPlans.map((item) =>
					item === plan
						? {
								...plan,
								quality: track.requestedQuality,
								variantId: track.requestedVariantId ?? undefined,
								liveAudioPreviewOffsetMs: track.liveAudioPreviewOffsetMs
							}
						: item
				);
				this.followActiveAudio(plan.cameraId);
			})
			.catch((restoreError) => {
				this.replaceTrack(plan.cameraId, {
					timingReconciliationNeeded: true,
					admissionError: 'Unable to restore the previous live timing.'
				});
				console.warn('Unable to restore the previous live timing', restoreError);
			});
	}

	private async releaseInactiveTracks(plans: LivePeerPlan[]): Promise<void> {
		try {
			await Promise.all(
				plans.flatMap((plan) => {
					const track = this.track(plan.cameraId);
					return !plan.active && track?.subscribed
						? [this.unsubscribeTrack(plan.cameraId, track.trackId)]
						: [];
				})
			);
		} catch (error) {
			const sessionToken = this.releaseLocalResources();
			this.error = 'Unable to release live streams';
			if (sessionToken !== null) {
				await this.#sessionTransport.delete(sessionToken).catch(() => {
					this.error = 'Live view stopped; server cleanup could not be confirmed';
				});
			}
			throw error;
		}
	}

	private async connect(
		plans: LivePeerPlan[],
		topologyKey: string,
		recovering: boolean
	): Promise<void> {
		const { peer, controlChannel } = this.preparePeer(topologyKey);
		try {
			const localTracks = this.prepareTracks(peer, plans);
			this.observePeer(peer);
			await this.negotiateTracks(peer, controlChannel, localTracks, recovering);
		} catch (error) {
			if (peer === this.#peer) {
				const sessionToken = this.releaseLocalResources();
				this.error = error instanceof Error ? error.message : 'Unable to start shared live view';
				if (sessionToken !== null) {
					try {
						await this.#sessionTransport.delete(sessionToken);
					} catch (closeError) {
						console.debug('Unable to close failed shared live session', closeError);
					}
				}
			}
			throw error;
		}
	}

	private preparePeer(topologyKey: string) {
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
		this.#talkbackTransceiver = peer.addTransceiver('audio', { direction: 'sendonly' });
		this.#topologyKey = topologyKey;
		this.connectionState = peer.connectionState;
		this.iceConnectionState = peer.iceConnectionState;
		this.error = null;
		this.#cameraByMid = {};
		this.#audioCameraByMid = {};
		this.#sourceSessionByCamera = {};
		this.#trackEventByMid = {};
		this.#capabilities = null;
		controlChannel.onmessage = (event) => this.receiveCurrentControl(peer, event);
		controlChannel.onclose = () => this.transportLost(peer);
		reliableChannel.onmessage = (event) => {
			if (peer === this.#peer) this.receiveMediaData(event);
		};

		return { peer, controlChannel };
	}

	private prepareTracks(peer: RTCPeerConnection, plans: LivePeerPlan[]): NegotiatedTrack[] {
		const localTracks = plans.map((plan, index) => {
			const trackId = `camera-${index}`;
			const transceiver = peer.addTransceiver('video', { direction: 'recvonly' });
			const audioTransceiver = peer.addTransceiver('audio', { direction: 'recvonly' });
			preferH265(transceiver);
			return {
				...plan,
				plan,
				trackId,
				audioTrackId: `audio-${index}`,
				transceiver,
				audioTransceiver
			};
		});

		this.tracks = Object.fromEntries(
			localTracks.map((track) => [
				track.cameraId,
				{
					cameraId: track.cameraId,
					trackId: track.trackId,
					audioTrackId: track.audioTrackId,
					receiver: null,
					stream: null,
					audioReceiver: null,
					audioStream: null,
					status: track.active ? 'connecting' : 'queued',
					requestedQuality: track.quality,
					requestedVariantId: track.variantId ?? null,
					activeStream: 'sub',
					pendingStream: null,
					estimatedBitrateBps: null,
					subscribed: false
				} satisfies LivePeerTrack
			])
		);

		return localTracks;
	}

	private observePeer(peer: RTCPeerConnection): void {
		peer.ontrack = (event) => {
			if (peer !== this.#peer || event.transceiver.mid === null) return;
			this.#trackEventByMid[event.transceiver.mid] = event;
			const cameraId =
				this.#cameraByMid[event.transceiver.mid] ?? this.#audioCameraByMid[event.transceiver.mid];
			if (!cameraId) return;
			if (event.track?.kind === 'audio') this.attachAudioTrackEvent(cameraId, event);
			else this.attachTrackEvent(cameraId, event);
		};
		peer.onconnectionstatechange = () => {
			if (peer !== this.#peer) return;
			this.connectionState = peer.connectionState;
			if (['failed', 'disconnected', 'closed'].includes(peer.connectionState)) {
				this.transportLost(peer);
			}
		};
		peer.oniceconnectionstatechange = () => {
			if (peer !== this.#peer) return;
			this.iceConnectionState = peer.iceConnectionState;
			if (['disconnected', 'failed', 'closed'].includes(peer.iceConnectionState)) {
				this.transportLost(peer);
			}
		};
	}

	private async negotiateTracks(
		peer: RTCPeerConnection,
		controlChannel: RTCDataChannel,
		localTracks: NegotiatedTrack[],
		recovering: boolean
	): Promise<void> {
		const offer = await peer.createOffer();
		await peer.setLocalDescription(offer);
		await waitForIceGathering(peer);

		if (peer !== this.#peer || !peer.localDescription) return;

		for (const track of localTracks) {
			if (track.transceiver.mid === null) {
				throw new Error(`No SDP MID assigned for ${track.cameraId}`);
			}
			if (track.audioTransceiver.mid === null) {
				throw new Error(`No audio SDP MID assigned for ${track.cameraId}`);
			}
		}

		const previous = this.#previousSession;
		let session: CreateResponse;
		try {
			session = await this.#sessionTransport.create(peer.localDescription, previous);
		} catch (error) {
			if (previous && this.#previousSession === previous && this.isAccessDenied(error)) {
				this.#previousSession = undefined;
				await this.#sessionTransport.delete(previous).catch(() => undefined);
			}
			throw error;
		}
		// A successful server acknowledgement consumes the one-use continuation even if
		// this opening peer was displaced while the HTTP response was in flight.
		if (this.#previousSession === previous) this.#previousSession = undefined;
		if (peer !== this.#peer) {
			await this.#sessionTransport.delete(session.session_id);
			return;
		}
		this.#sessionToken = session.session_id;
		this.sessionId = session.session_id;
		await peer.setRemoteDescription({
			type: session.answer.type as RTCSdpType,
			sdp: session.answer.sdp
		});
		// ponytail: Start the opening timeout only after signaling succeeds.
		await waitForDataChannel(controlChannel);
		if (peer !== this.#peer) return;
		await this.waitForCapabilities();
		if (peer !== this.#peer) return;
		await Promise.all(
			localTracks
				.filter((track) => track.active)
				.map((track) => this.admitConnectedTrack(track.plan, track.trackId, recovering))
		);
	}

	private async admitConnectedTrack(plan: LivePeerPlan, id: string, recovering: boolean) {
		const owner = this.#peer;
		try {
			await this.subscribeTrack(
				plan,
				id,
				plan.quality,
				plan.variantId ?? null,
				false,
				plan.liveAudioPreviewOffsetMs
			);
		} catch (error) {
			if (error instanceof SupersededPreview) {
				this.replaceTrack(plan.cameraId, { timingReconciliationNeeded: true });
				return;
			}
			if (
				!recovering ||
				plan.liveAudioPreviewOffsetMs === undefined ||
				owner !== this.#peer ||
				!this.#desiredPlans.includes(plan)
			)
				throw error;
			const saved = { ...plan, variantId: undefined, liveAudioPreviewOffsetMs: undefined };
			// The preview may have admitted video before audio failed. Replace the same
			// subscription in place so its MID retains the existing sender clock.
			await this.subscribeTrack(plan, id, saved.quality, null, true, undefined, true);
			if (owner === this.#peer && this.#desiredPlans.includes(plan)) {
				this.#desiredPlans = this.#desiredPlans.map((item) => (item === plan ? saved : item));
				this.replaceTrack(plan.cameraId, { restoredSavedTiming: true });
				this.followActiveAudio(plan.cameraId);
			}
		}
	}

	cancelDisconnectedPreview(cameraId: string): void {
		this.setDesiredPlans(
			this.#desiredPlans.map((plan) =>
				plan.cameraId === cameraId
					? { ...plan, variantId: undefined, liveAudioPreviewOffsetMs: undefined }
					: plan
			)
		);
		if (this.#peer) {
			void this.enqueue(() => this.configureNow(this.#desiredPlans, true)).catch(() =>
				this.scheduleRecovery()
			);
		} else {
			this.scheduleRecovery();
		}
	}

	private async subscribeTrack(
		plan: LivePeerPlan,
		subscriptionId: string,
		quality: LiveQuality,
		variantId: 'main' | 'sub' | null = null,
		replacement = false,
		liveAudioPreviewOffsetMs?: number,
		confirmTiming = liveAudioPreviewOffsetMs !== undefined
	): Promise<void> {
		const { cameraId } = plan;
		const owner = this.#peer;
		const sourceSessionId = this.#sourceSessionByCamera[cameraId];
		if (!sourceSessionId) {
			if (confirmTiming) throw new Error('The live camera is unavailable for timing calibration.');
			if (this.#capabilities?.cameras.some((camera) => camera.sourceId === cameraId)) {
				throw new Error(`Camera ${cameraId} has no live source session.`);
			}
			this.replaceTrack(cameraId, {
				status: 'unavailable',
				subscribed: false,
				stream: null,
				receiver: null,
				pendingStream: null
			});
			return;
		}
		if (!replacement) this.replaceTrack(cameraId, { status: 'connecting' });
		const subscribe = create(SubscribeMediaSchema, {
			subscriptionId,
			sourceSessionId,
			kind: MediaKind.VIDEO,
			requestedDeliveryTransport: DeliveryTransport.RTP,
			videoQuality: protoQuality(variantId === null ? quality : 'auto'),
			liveAudioPreviewOffsetMs,
			variantId: variantId ?? ''
		});
		const result = await this.requestMedia(cameraId, subscribe);
		if (owner !== this.#peer) {
			if (confirmTiming) throw new Error('The live timing session changed.');
			return;
		}
		if (result === null) {
			if (confirmTiming) throw new Error('Video timing preview was not admitted.');
			return;
		}
		if (result.case !== 'subscriptionResult' || result.value.delivery.case !== 'rtp') {
			throw new Error('Server returned an unexpected media subscription response.');
		}
		if (confirmTiming && !timingConfirmed(result.value.liveAudio, liveAudioPreviewOffsetMs)) {
			throw new Error('The server did not apply the video timing preview.');
		}
		this.applyVideoAdmission(
			cameraId,
			quality,
			variantId,
			replacement,
			result.value.delivery.value.mid,
			result.value
		);
		await this.completeTimingAdmission(plan, variantId, liveAudioPreviewOffsetMs, confirmTiming);
	}

	private async completeTimingAdmission(
		plan: LivePeerPlan,
		variantId: 'main' | 'sub' | null,
		liveAudioPreviewOffsetMs: number | undefined,
		confirmTiming: boolean
	) {
		const { cameraId } = plan;
		if (liveAudioPreviewOffsetMs !== undefined) {
			if (variantId === null) throw new Error('Choose a video profile for timing preview.');
			await this.waitForPreviewProfile(plan, variantId);
		}
		await this.subscribeAudio(
			cameraId,
			this.track(cameraId)?.audioTrackId ?? `audio-${cameraId}`,
			liveAudioPreviewOffsetMs === undefined ? null : variantId,
			liveAudioPreviewOffsetMs,
			confirmTiming
		);
		this.replaceTrack(cameraId, {
			liveAudioPreviewOffsetMs,
			timingReconciliationNeeded: false,
			restoredSavedTiming: false
		});
		this.followActiveAudio(cameraId);
	}

	private applyVideoAdmission(
		cameraId: string,
		quality: LiveQuality,
		variantId: 'main' | 'sub' | null,
		replacement: boolean,
		mid: string,
		admission: SubscriptionResult
	) {
		const selectedStream = liveVariant(admission.selectedVariantId);
		if (selectedStream === null) throw new Error('Server selected an unsupported live variant.');
		const current = this.track(cameraId);
		const pendingStream =
			current !== null &&
			(replacement || !current.subscribed) &&
			current.activeStream !== selectedStream
				? selectedStream
				: null;
		this.#cameraByMid[mid] = cameraId;
		this.replaceTrack(cameraId, {
			admissionError: null,
			liveAudio: admission.liveAudio,
			requestedQuality: quality,
			requestedVariantId: variantId,
			activeStream: pendingStream === null ? selectedStream : current!.activeStream,
			pendingStream,
			subscribed: true
		});
		this.applyLiveQuality(cameraId, admission.liveQuality);
		const event = this.#trackEventByMid[mid];
		if (event) this.attachTrackEvent(cameraId, event);
	}

	private waitForPreviewProfile(plan: LivePeerPlan, desired: 'main' | 'sub'): Promise<void> {
		const { cameraId } = plan;
		return new Promise((resolve, reject) => {
			const timer = setTimeout(
				() => observe(new Error('The selected video profile did not become active in time.')),
				controlTimeoutMs
			);
			const observe = (failure?: Error) => {
				const requested = this.#desiredPlans.find((item) => item.cameraId === cameraId);
				if (
					!failure &&
					(!requested?.active ||
						requested.variantId !== plan.variantId ||
						requested.liveAudioPreviewOffsetMs !== plan.liveAudioPreviewOffsetMs)
				)
					failure = new SupersededPreview();
				const track = this.track(cameraId);
				if (
					!failure &&
					track?.liveQuality?.selectionReason === LiveQualitySelectionReason.VARIANT_UNAVAILABLE
				)
					failure = new Error('The selected video profile is unavailable for this viewer.');
				if (
					!failure &&
					(track?.confirmedVideoVariant !== desired ||
						(track.pendingStream !== null && track.pendingStream !== desired))
				)
					return;
				clearTimeout(timer);
				this.#previewWaiters.delete(cameraId);
				if (failure) reject(failure);
				else resolve();
			};
			this.#previewWaiters.set(cameraId, observe);
			observe();
		});
	}

	private async subscribeAudio(
		cameraId: string,
		subscriptionId: string,
		videoVariant: 'main' | 'sub' | null,
		previewOffset?: number,
		confirmTiming = previewOffset !== undefined
	): Promise<void> {
		const lifetime = this.#peerLifetime.signal;
		const sourceSessionId = this.#sourceSessionByCamera[cameraId];
		if (!sourceSessionId) return;
		const owner = this.#peer;
		const source = this.#capabilities?.sourceSessions.find(
			(item) => item.sourceSessionId === sourceSessionId
		);
		const current = this.track(cameraId);
		const variant =
			source?.audio?.variants.find(
				(item) => item.variantId === (videoVariant ?? current?.activeStream)
			) ?? (previewOffset === undefined ? source?.audio?.variants[0] : undefined);
		if (!variant) {
			this.replaceTrack(cameraId, { audioAdmission: undefined });
			if (previewOffset !== undefined)
				throw new Error('This stream has no matching live audio profile.');
			if (confirmTiming) {
				await this.unsubscribeAudio(cameraId, subscriptionId);
			}
			return;
		}
		const delivery = requestedAudioDelivery(variant);
		if (
			current?.audioSourceSessionId === sourceSessionId &&
			current?.audioVariantId === variant.variantId &&
			current?.audioDelivery === delivery &&
			previewOffset === undefined &&
			!confirmTiming
		) {
			return;
		}
		if (
			current?.audioVariantId &&
			(current.audioVariantId !== variant.variantId || current.audioDelivery !== delivery)
		) {
			await this.unsubscribeAudio(cameraId, subscriptionId);
		}
		lifetime.throwIfAborted();
		const requestedDeliveryTransport =
			delivery === 'mediaData' ? DeliveryTransport.RELIABLE_DATA : DeliveryTransport.RTP;
		const result = await this.requestMedia(
			cameraId,
			create(SubscribeMediaSchema, {
				subscriptionId,
				sourceSessionId,
				kind: MediaKind.AUDIO,
				liveAudioPreviewOffsetMs: previewOffset,
				requestedDeliveryTransport,
				videoQuality: VideoQuality.AUTO,
				variantId: variant.variantId
			})
		);
		lifetime.throwIfAborted();
		if (
			owner !== this.#peer ||
			this.#sourceSessionByCamera[cameraId] !== sourceSessionId ||
			result?.case !== 'subscriptionResult'
		) {
			if (confirmTiming) throw new Error('Audio timing preview was not admitted.');
			return;
		}
		this.replaceTrack(cameraId, {
			audioAdmission: 'admitted',
			audioSourceSessionId: sourceSessionId,
			audioVariantId: variant.variantId,
			audioDelivery: result.value.delivery.case,
			admissionError: null
		});
		if (confirmTiming && !timingConfirmed(result.value.liveAudio, previewOffset)) {
			throw new Error('The server did not apply the audio timing preview.');
		}
		await this.attachAudioDelivery(
			cameraId,
			sourceSessionId,
			variant,
			result.value.delivery,
			previewOffset !== undefined
		);
		lifetime.throwIfAborted();
		this.replaceTrack(cameraId, { audioTiming: result.value.liveAudio });
	}

	private async unsubscribeAudio(cameraId: string, subscriptionId: string): Promise<void> {
		const lifetime = this.#peerLifetime.signal;
		await this.request({
			case: 'unsubscribe',
			value: create(UnsubscribeSchema, { subscriptionIds: [subscriptionId] })
		});
		lifetime.throwIfAborted();
		this.releaseAudio(cameraId);
	}

	private async attachAudioDelivery(
		cameraId: string,
		sourceSessionId: string,
		variant: MediaVariantCapability,
		delivery: SubscriptionResult['delivery'],
		requirePlayback: boolean
	): Promise<void> {
		const owner = this.#peer;
		if (delivery.case === 'rtp') {
			const mid = delivery.value.mid;
			this.#audioCameraByMid[mid] = cameraId;
			const event = this.#trackEventByMid[mid];
			if (event) this.attachAudioTrackEvent(cameraId, event);
			return;
		}
		const format = variant.format?.format;
		if (
			delivery.case !== 'mediaData' ||
			variant.codec?.name.toLowerCase() !== 'aac' ||
			format?.case !== 'audio'
		) {
			if (requirePlayback)
				throw new Error('This audio delivery cannot be previewed in this viewer.');
			return;
		}
		if (this.#audioPlaybackByBinding.has(delivery.value.streamBindingId)) return;
		this.releaseAudioDecoder(cameraId);
		let playback: AacAudioPlayback | undefined;
		try {
			this.#audioContext ??= new AudioContext();
			playback = new AacAudioPlayback(this.#audioContext);
			await playback.configure(
				variant.codec?.name ?? 'aac',
				format.value.decoderConfig,
				format.value.sampleRateHz,
				format.value.channelCount
			);
			if (owner !== this.#peer || this.track(cameraId)?.audioSourceSessionId !== sourceSessionId) {
				playback.close();
				return;
			}
			this.#audioPlaybackByBinding.set(delivery.value.streamBindingId, playback);
			this.#audioBindingByCamera[cameraId] = delivery.value.streamBindingId;
		} catch (error) {
			playback?.close();
			if (requirePlayback)
				throw new Error('This browser cannot decode the audio timing preview.', { cause: error });
			console.warn(`AAC playback is unavailable for ${cameraId}`, error);
		}
	}

	private async requestMedia(
		cameraId: string,
		subscribe: SubscribeMedia
	): Promise<Ok['result'] | null> {
		try {
			return await this.request({ case: 'subscribeMedia', value: subscribe });
		} catch (error) {
			if (!(error instanceof MediaAdmissionError)) throw error;
			if (subscribe.kind === MediaKind.AUDIO) {
				this.replaceTrack(cameraId, { audioAdmission: 'retry' });
				return null;
			}
			this.replaceTrack(cameraId, {
				status: 'unavailable',
				admissionError: 'Server did not admit this stream'
			});
			return null;
		}
	}

	private async unsubscribeTrack(cameraId: string, subscriptionId: string): Promise<void> {
		const audioTrackId = this.track(cameraId)?.audioTrackId;
		await this.request({
			case: 'unsubscribe',
			value: create(UnsubscribeSchema, {
				subscriptionIds: audioTrackId ? [subscriptionId, audioTrackId] : [subscriptionId]
			})
		});
		this.releaseAudio(cameraId);
		this.clearAudioSync(cameraId);
		this.#previewWaiters.get(cameraId)?.(new Error('The video subscription closed.'));
		this.replaceTrack(cameraId, {
			confirmedVideoVariant: undefined,
			status: 'queued',
			subscribed: false,
			pendingStream: null,
			estimatedBitrateBps: null,
			liveQuality: undefined,
			liveAudio: undefined,
			liveAudioPreviewOffsetMs: undefined
		});
	}

	private releaseAudioDecoder(cameraId: string): void {
		const binding = this.#audioBindingByCamera[cameraId];
		if (binding) {
			this.#audioPlaybackByBinding.get(binding)?.close();
			this.#audioPlaybackByBinding.delete(binding);
			delete this.#audioBindingByCamera[cameraId];
		}
	}

	private releaseAudio(cameraId: string): void {
		this.releaseAudioDecoder(cameraId);
		for (const [mid, sourceId] of Object.entries(this.#audioCameraByMid)) {
			if (sourceId === cameraId) delete this.#audioCameraByMid[mid];
		}
		this.replaceTrack(cameraId, {
			audioSourceSessionId: undefined,
			audioVariantId: undefined,
			audioDelivery: undefined,
			audioTiming: undefined,
			audioReceiver: null,
			audioStream: null,
			audioAdmission: undefined
		});
	}

	private releaseCameraAudio(cameraId: string): void {
		this.releaseAudio(cameraId);
	}

	private attachTrackEvent(cameraId: string, event: RTCTrackEvent): void {
		const current = this.track(cameraId);
		const mediaTrack = event.track ?? event.receiver.track;
		const stream =
			current?.receiver === event.receiver && current.stream
				? current.stream
				: mediaTrack
					? new MediaStream([mediaTrack])
					: (event.streams[0] ?? new MediaStream());
		this.replaceTrack(cameraId, { receiver: event.receiver, stream, status: 'live' });
	}

	private attachAudioTrackEvent(cameraId: string, event: RTCTrackEvent): void {
		const mediaTrack = event.track ?? event.receiver.track;
		const stream = mediaTrack
			? new MediaStream([mediaTrack])
			: (event.streams[0] ?? new MediaStream());
		this.replaceTrack(cameraId, { audioReceiver: event.receiver, audioStream: stream });
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
		if (reply.result.case === 'error') {
			const ErrorType = command.case === 'subscribeMedia' ? MediaAdmissionError : Error;
			throw new ErrorType(reply.result.value.message);
		}
		if (reply.result.case !== 'ok') throw new Error('Server returned an empty media response.');
		return reply.result.value.result;
	}

	private receiveCurrentControl(peer: RTCPeerConnection, event: MessageEvent): void {
		if (peer === this.#peer) this.receiveControl(event);
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
			if (event.case === 'talkbackState') this.receiveTalkbackState(event.value);
			if (event.case === 'cameraUpdated') this.receiveCameraUpdate(event.value);
			if (event.case === 'initialCapabilities') {
				this.receiveCapabilities(event.value);
			}
			if (event.case === 'connectionUpdate') {
				this.receiveNetworkEstimate(event.value.availableBitrateBps);
			}
			if (event.case === 'subscriptionStreamState') {
				this.receiveStreamState(event.value);
			}
			if (event.case === 'sourceSessionRemoved') {
				this.removeSourceDiagnostics(event.value.sourceSessionId);
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

	private acceptsTelemetry(): boolean {
		return (
			this.#peer !== null &&
			!['failed', 'disconnected', 'closed'].includes(this.connectionState) &&
			!['failed', 'disconnected', 'closed'].includes(this.iceConnectionState)
		);
	}

	private receiveNetworkEstimate(bitsPerSecond: bigint | undefined): void {
		if (this.acceptsTelemetry()) this.estimatedBitrateBps = safeBitrate(bitsPerSecond);
	}

	private applyLiveQuality(
		cameraId: string,
		liveQuality: LiveQualityDiagnostics | undefined
	): void {
		const current = this.track(cameraId);
		if (!this.acceptsTelemetry() || !current?.subscribed || !this.#sourceSessionByCamera[cameraId])
			return;
		this.replaceTrack(cameraId, {
			liveQuality,
			...(liveQuality === undefined
				? {}
				: {
						confirmedVideoVariant: liveVariant(liveQuality.activeVariantId) ?? undefined,
						activeStream: liveVariant(liveQuality.activeVariantId) ?? current.activeStream,
						pendingStream: liveVariant(liveQuality.pendingVariantId)
					})
		});
		this.#previewWaiters.get(cameraId)?.();
		if (liveQuality !== undefined) this.receiveNetworkEstimate(liveQuality.sessionBandwidthBps);
	}

	private receiveStreamState(update: SubscriptionStreamState): void {
		if (!this.acceptsTelemetry()) return;
		const audio = Object.values(this.tracks).find(
			(track) =>
				track.subscribed && track.audioVariantId && track.audioTrackId === update.subscriptionId
		);
		if (audio && update.liveAudio && update.activeVariantId === audio.audioVariantId) {
			this.replaceTrack(audio.cameraId, { audioTiming: update.liveAudio });
			return;
		}
		const current = Object.values(this.tracks).find(
			(track) => track.trackId === update.subscriptionId && track.subscribed
		);
		if (!current || !this.#sourceSessionByCamera[current.cameraId]) return;
		if (update.liveAudio) this.replaceTrack(current.cameraId, { liveAudio: update.liveAudio });
		if (update.liveQuality !== undefined) {
			this.applyLiveQuality(current.cameraId, update.liveQuality);
			this.followActiveAudio(current.cameraId);
			return;
		}
		const activeStream = liveVariant(update.activeVariantId);
		if (activeStream === null) return;
		this.replaceTrack(current.cameraId, {
			activeStream,
			confirmedVideoVariant: activeStream,
			pendingStream: current.pendingStream === activeStream ? null : current.pendingStream,
			liveQuality: undefined
		});
		this.#previewWaiters.get(current.cameraId)?.();
		this.followActiveAudio(current.cameraId);
	}

	private clearAudioSync(cameraId: string): void {
		const previous = this.#audioSync.get(cameraId);
		if (previous?.retry) clearTimeout(previous.retry);
		this.#audioSync.delete(cameraId);
	}

	private calibratingAudio(cameraId: string): boolean {
		return (
			this.#previewWaiters.has(cameraId) ||
			this.track(cameraId)?.liveAudioPreviewOffsetMs !== undefined ||
			this.#desiredPlans.some(
				(plan) => plan.cameraId === cameraId && plan.liveAudioPreviewOffsetMs !== undefined
			)
		);
	}

	private followActiveAudio(cameraId: string, refresh = false): void {
		const track = this.track(cameraId);
		const source = this.#capabilities?.sourceSessions.find(
			(item) => item.sourceSessionId === this.#sourceSessionByCamera[cameraId]
		);
		const variants = source?.audio?.variants;
		const variant = variants?.find((v) => v.variantId === track?.activeStream) ?? variants?.[0];
		if (
			!track?.subscribed ||
			this.calibratingAudio(cameraId) ||
			!variant ||
			(track.audioVariantId === variant.variantId &&
				track.audioDelivery === requestedAudioDelivery(variant))
		) {
			this.clearAudioSync(cameraId);
			return;
		}
		const delivery = requestedAudioDelivery(variant);
		let state = this.#audioSync.get(cameraId);
		if (state?.variant !== variant.variantId || state.delivery !== delivery) {
			this.clearAudioSync(cameraId);
			state = { variant: variant.variantId, delivery, attempts: 0, busy: false };
			this.#audioSync.set(cameraId, state);
		}
		if (refresh && !state.busy && !state.retry && state.attempts >= 3) state.attempts = 0;
		if (state.busy || state.retry || state.attempts >= 3) return;
		const pending = state;
		pending.busy = true;
		void this.enqueue(async () => {
			if (this.#audioSync.get(cameraId) !== pending) return;
			const latest = this.track(cameraId);
			if (!latest?.subscribed || this.calibratingAudio(cameraId)) return;
			pending.attempts++;
			await this.subscribeAudio(cameraId, latest.audioTrackId, latest.activeStream);
		})
			.catch((error) => console.warn('Unable to follow live audio profile', error))
			.finally(() => {
				if (this.#audioSync.get(cameraId) !== pending) {
					this.followActiveAudio(cameraId);
					return;
				}
				pending.busy = false;
				const admitted = this.track(cameraId);
				if (
					admitted?.audioVariantId === pending.variant &&
					admitted.audioDelivery === pending.delivery
				) {
					this.clearAudioSync(cameraId);
				} else if (pending.attempts < 3) {
					pending.retry = setTimeout(() => {
						pending.retry = undefined;
						this.followActiveAudio(cameraId);
					}, 250 * pending.attempts);
				}
			});
	}

	private removeSourceDiagnostics(sourceSessionId: string): void {
		for (const [cameraId, source] of Object.entries(this.#sourceSessionByCamera)) {
			if (source !== sourceSessionId) continue;
			delete this.#sourceSessionByCamera[cameraId];
			this.releaseCameraAudio(cameraId);
			this.replaceTrack(cameraId, { liveQuality: undefined, pendingStream: null });
		}
	}

	private receiveCameraUpdate(update: CameraUpdated): void {
		const capabilities = replaceKnownCamera(this.#capabilities, update);
		if (!capabilities || !update.camera) return;
		this.#capabilities = capabilities;
		const assets = audioAssets(update.camera);
		if (assets) this.cameraAudioAssets = { ...this.cameraAudioAssets, [update.sourceId]: assets };
		else {
			const next = { ...this.cameraAudioAssets };
			delete next[update.sourceId];
			this.cameraAudioAssets = next;
		}
		this.updateTalkbackGroups(capabilities.cameras);
	}

	private updateTalkbackGroups(cameras: readonly CameraInfo[]): void {
		const groups = new Set<string>();
		for (const camera of cameras) {
			if (!camera.deviceCapabilities?.twoWayAudio) continue;
			for (const groupId of camera.groupIds) groups.add(groupId);
		}
		const ids = [...groups].sort((left, right) => left.localeCompare(right));
		if (
			ids.length !== this.talkbackGroups.length ||
			ids.some((id, index) => this.talkbackGroups[index].id !== id)
		) {
			this.talkbackGroups = ids.map((id) => ({ id, label: id }));
		}
	}

	private receiveCapabilities(capabilities: ServerCapabilities): void {
		this.#capabilities = capabilities;
		if (capabilities.accessSession?.role === AccessRole.ADMINISTRATOR) {
			this.#talkbackAuthorization ??= Symbol();
		} else {
			this.#talkbackAuthorization = null;
			this.releaseMicrophone();
		}
		this.cameraAudioAssets = Object.fromEntries(
			capabilities.cameras.flatMap((camera) => {
				const assets = audioAssets(camera);
				return assets ? [[camera.sourceId, assets]] : [];
			})
		);
		this.updateTalkbackGroups(capabilities.cameras);
		this.#sourceSessionByCamera = Object.fromEntries(
			capabilities.sourceSessions
				.filter(
					(source) =>
						source.sourceId.length > 0 && (source.video !== undefined || source.audio !== undefined)
				)
				.map((source) => [source.sourceId, source.sourceSessionId])
		);
		for (const track of Object.values(this.tracks)) {
			if (
				track.audioSourceSessionId &&
				this.#sourceSessionByCamera[track.cameraId] !== track.audioSourceSessionId
			)
				this.releaseCameraAudio(track.cameraId);
		}
		for (const waiter of this.#capabilitiesWaiters) {
			clearTimeout(waiter.timeout);
			waiter.resolve(capabilities);
		}
		this.#capabilitiesWaiters = [];
		for (const track of Object.values(this.tracks)) {
			if (
				track.subscribed &&
				(track.audioVariantId !== undefined || track.audioAdmission === 'retry')
			)
				this.followActiveAudio(track.cameraId);
		}
	}

	private receiveMediaData(event: MessageEvent): void {
		if (!(event.data instanceof ArrayBuffer)) return;
		try {
			const message = fromBinary(MessageSchema, new Uint8Array(event.data));
			if (message.message.case !== 'audio' || message.message.value.message.case !== 'frame')
				return;
			const frame = message.message.value.message.value;
			this.#audioPlaybackByBinding.get(frame.streamBindingId)?.push(frame);
		} catch (error) {
			console.debug('Ignoring invalid WebRTC media data', error);
		}
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
		for (const waiter of this.#previewWaiters.values()) waiter(new Error(message));
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
		const sessionToken = this.#sessionToken;
		this.#closingPeer = this.#peer;
		const subscriptionIds = Object.values(this.tracks)
			.filter((track) => track.subscribed)
			.flatMap((track) => [track.trackId, track.audioTrackId]);
		if (this.#controlChannel?.readyState === 'open' && subscriptionIds.length > 0) {
			try {
				await this.request({
					case: 'unsubscribe',
					value: create(UnsubscribeSchema, { subscriptionIds })
				});
			} catch {
				// Session deletion remains authoritative teardown.
			}
		}
		this.releaseLocalResources();
		if (sessionToken) {
			await this.#sessionTransport
				.delete(sessionToken)
				.catch((error) => console.debug('Unable to close shared live session', error));
		}
	}

	private releaseLocalResources(): string | null {
		this.#peerLifetime.abort(
			new Error('The live connection changed. Send the command again if needed.')
		);
		this.#peerLifetime = new AbortController();
		for (const id of this.#audioSync.keys()) this.clearAudioSync(id);
		const peer = this.#peer;
		const sessionToken = this.#sessionToken;
		this.#peer = null;
		this.#closingPeer = null;
		this.#sessionToken = null;
		this.#topologyKey = '';
		this.#cameraByMid = {};
		this.#audioCameraByMid = {};
		for (const playback of this.#audioPlaybackByBinding.values()) playback.close();
		this.#audioPlaybackByBinding.clear();
		void this.#audioContext?.close();
		this.#audioContext = null;
		this.#audioBindingByCamera = {};
		this.#sourceSessionByCamera = {};
		this.#trackEventByMid = {};
		this.#capabilities = null;
		this.#talkbackAuthorization = null;
		this.talkbackGroups = [];
		this.cameraAudioAssets = {};
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
		this.#talkbackTransceiver = null;
		this.releaseMicrophone();
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
}

function requestedAudioDelivery(variant: MediaVariantCapability): AudioDelivery {
	return variant.deliveryTransports.includes(DeliveryTransport.RELIABLE_DATA) ? 'mediaData' : 'rtp';
}

function timingConfirmed(
	timing: LiveAudioTiming | undefined,
	preview: number | undefined
): boolean {
	if (!timing) return false;
	return preview === undefined
		? !timing.preview && timing.appliedOffsetMs === timing.configuredOffsetMs
		: timing.preview && timing.appliedOffsetMs === preview;
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

function talkbackTarget(selection: TalkbackTargetSelection): TalkbackTarget {
	if ('sourceId' in selection) {
		return create(TalkbackTargetSchema, {
			selection: { case: 'sourceId', value: selection.sourceId }
		});
	}
	if ('groupId' in selection) {
		return create(TalkbackTargetSchema, {
			selection: { case: 'groupId', value: selection.groupId }
		});
	}
	return create(TalkbackTargetSchema, {
		selection: { case: 'all', value: create(AllTalkbackTargetsSchema) }
	});
}

function waitForDataChannel(channel: RTCDataChannel): Promise<void> {
	if (channel.readyState === 'open') return Promise.resolve();
	const closed = () => new Error(`WebRTC ${channel.label} channel closed before opening.`);
	if (channel.readyState === 'closing' || channel.readyState === 'closed')
		return Promise.reject(closed());
	return new Promise((resolve, reject) => {
		const previousClose = channel.onclose;
		const timeout = setTimeout(
			() => finish(new Error(`WebRTC ${channel.label} channel did not open.`)),
			controlTimeoutMs
		);
		const finish = (error?: Error) => {
			clearTimeout(timeout);
			channel.onopen = null;
			channel.onerror = null;
			channel.onclose = previousClose;
			if (error) reject(error);
			else resolve();
		};
		channel.onopen = () => finish();
		channel.onerror = () => finish(new Error(`WebRTC ${channel.label} channel failed.`));
		channel.onclose = (event) => {
			finish(closed());
			previousClose?.call(channel, event);
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

function acquireTalkbackMicrophone(signal: AbortSignal): Promise<MediaStream> {
	return new Promise((resolve, reject) => {
		const abort = () => reject(signal.reason);
		if (signal.aborted) {
			abort();
			return;
		}
		signal.addEventListener('abort', abort, { once: true });
		navigator.mediaDevices
			.getUserMedia({ audio: true })
			.then((stream) => {
				if (signal.aborted) stream.getTracks().forEach((track) => track.stop());
				else resolve(stream);
			}, reject)
			.finally(() => signal.removeEventListener('abort', abort));
	});
}
