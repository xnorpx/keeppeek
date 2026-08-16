import {
	closeBrowserLiveSession,
	createBrowserLiveSession,
	setBrowserLiveTrackQuality
} from './api';
import type { LiveQuality } from './types';

export type LivePeerPlan = {
	cameraId: string;
	quality: LiveQuality;
};

export type LivePeerTrack = {
	cameraId: string;
	trackId: string;
	receiver: RTCRtpReceiver | null;
	stream: MediaStream | null;
	status: 'connecting' | 'live' | 'unavailable';
	requestedQuality: LiveQuality;
	activeStream: 'main' | 'sub';
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
	#cameraByMid: Record<string, string> = {};
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
		const sessionId = this.releaseLocalResources();
		if (sessionId === null) return;
		navigator.sendBeacon(`/api/live/browser/${sessionId}/close`, '');
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
				.filter((plan) => this.track(plan.cameraId)?.requestedQuality !== plan.quality)
				.map((plan) => this.setQuality(plan.cameraId, plan.quality))
		);
	}

	private async connect(plans: LivePeerPlan[], topologyKey: string): Promise<void> {
		const peer = new RTCPeerConnection();
		this.#peer = peer;
		this.#topologyKey = topologyKey;
		this.connectionState = peer.connectionState;
		this.iceConnectionState = peer.iceConnectionState;
		this.error = null;
		this.#cameraByMid = {};

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
					activeStream: 'sub',
					estimatedBitrateBps: null
				} satisfies LivePeerTrack
			])
		);

		peer.ontrack = (event) => {
			if (peer !== this.#peer || event.transceiver.mid === null) return;
			const cameraId = this.#cameraByMid[event.transceiver.mid];
			if (!cameraId) return;
			const mediaTrack = event.track ?? event.receiver.track;
			const stream = mediaTrack
				? new MediaStream([mediaTrack])
				: (event.streams[0] ?? new MediaStream());
			this.replaceTrack(cameraId, { receiver: event.receiver, stream, status: 'live' });
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

			const tracks = localTracks.map((track) => {
				const mid = track.transceiver.mid;
				if (mid === null) throw new Error(`No SDP MID assigned for ${track.cameraId}`);
				this.#cameraByMid[mid] = track.cameraId;
				return {
					camera_id: track.cameraId,
					track_id: track.trackId,
					mid,
					quality: track.quality
				};
			});

			const session = await createBrowserLiveSession(tracks, peer.localDescription);
			if (peer !== this.#peer) {
				await closeBrowserLiveSession(session.session_id);
				return;
			}

			this.sessionId = session.session_id;
			await peer.setRemoteDescription(session.answer as RTCSessionDescriptionInit);
		} catch (error) {
			if (peer === this.#peer) {
				const sessionId = this.releaseLocalResources();
				this.error = error instanceof Error ? error.message : 'Unable to start shared live view';
				if (sessionId !== null) {
					try {
						await closeBrowserLiveSession(sessionId);
					} catch (closeError) {
						console.debug('Unable to close failed shared live session', closeError);
					}
				}
			}
			throw error;
		}
	}
	private async setQuality(cameraId: string, quality: LiveQuality): Promise<void> {
		const track = this.tracks[cameraId];
		if (!track) return;
		const sessionId = this.sessionId;
		if (sessionId === null) return;
		await setBrowserLiveTrackQuality(sessionId, track.trackId, quality);
		if (sessionId !== this.sessionId) return;
		this.replaceTrack(cameraId, { requestedQuality: quality });
	}

	private replaceTrack(cameraId: string, update: Partial<LivePeerTrack>): void {
		const current = this.track(cameraId);
		if (!current) return;
		this.tracks = { ...this.tracks, [cameraId]: { ...current, ...update } };
	}

	private async closeNow(): Promise<void> {
		const sessionId = this.releaseLocalResources();
		if (sessionId === null) return;
		try {
			await closeBrowserLiveSession(sessionId);
		} catch (error) {
			console.debug('Unable to close shared live session', error);
		}
	}

	private releaseLocalResources(): number | null {
		const peer = this.#peer;
		const sessionId = this.sessionId;
		this.#peer = null;
		this.#topologyKey = '';
		this.#cameraByMid = {};
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
		peer?.close();
		this.tracks = {};
		this.connectionState = peer ? 'closed' : 'new';
		this.iceConnectionState = 'closed';
		return sessionId;
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
