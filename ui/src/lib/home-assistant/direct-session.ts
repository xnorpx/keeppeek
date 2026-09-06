import type { ServerCapabilities } from '../proto/webrtc_pb';
import type { CardConfig, CardSource } from './config';
import {
	sourceKey,
	type SessionAdapter,
	type SessionSnapshot,
	type SourceChoice,
	type StreamState
} from './connection-manager';
import { CardConnectionError, connectionError } from './direct-http';
import { CardSubscriptionError, DirectPeer } from './direct-peer';

type PeerAdapter = Pick<DirectPeer, 'open' | 'close' | 'subscribe' | 'unsubscribe' | 'stream'>;
type PeerFactory = (callbacks: ConstructorParameters<typeof DirectPeer>[1]) => PeerAdapter;
type Binding = {
	signature: string;
	subscriptionId: string | null;
	mid: string | null;
	state: StreamState;
};

export class DirectSession implements SessionAdapter {
	#factory: PeerFactory;
	#notify: (snapshot: SessionSnapshot) => void;
	#peer: PeerAdapter | null = null;
	#capabilities: ServerCapabilities | null = null;
	#sources: readonly CardSource[] = [];
	#bindings = new Map<string, Binding>();
	#sequence = 0;
	#status: SessionSnapshot['status'] = 'connecting';
	#message: string | null = null;
	#dirty = false;
	#working: Promise<void> | null = null;
	#recovering: Promise<void> | null = null;
	#timer: ReturnType<typeof setTimeout> | undefined;
	#attempts = 0;
	#retryImmediately = false;
	#terminal = false;
	#closed = false;

	constructor(
		config: Pick<CardConfig, 'endpoint' | 'token'>,
		notify: (snapshot: SessionSnapshot) => void,
		factory?: PeerFactory
	) {
		this.#factory = factory ?? ((callbacks) => new DirectPeer(config, callbacks));
		this.#notify = notify;
	}

	configure(sources: readonly CardSource[]): void {
		this.#sources = [...sources];
		this.schedule();
	}

	private schedule(): void {
		this.#dirty = true;
		if (this.#working || this.#recovering || this.#timer || this.#terminal || this.#closed) return;
		this.#working = Promise.resolve()
			.then(() => this.synchronize())
			.catch((error: unknown) => this.recover(error))
			.finally(() => {
				this.#working = null;
				if (this.#dirty) this.schedule();
			});
	}

	private async synchronize(): Promise<void> {
		this.#dirty = false;
		if (this.#closed) return;
		if (!this.#peer) await this.open();
		const peer = this.#peer;
		if (!peer || this.#closed) return;
		const desired = new Set(this.#sources.map(sourceKey));
		for (const [key, binding] of this.#bindings) {
			if (desired.has(key)) continue;
			this.#bindings.delete(key);
			if (binding.subscriptionId) await peer.unsubscribe(binding.subscriptionId);
			if (peer !== this.#peer) return;
		}
		await Promise.all(this.#sources.map((source) => this.synchronizeSource(peer, source)));
		if (peer === this.#peer) this.emit();
	}

	private async open(): Promise<void> {
		const peer: PeerAdapter = this.#factory({
			capabilities: (capabilities) => {
				if (peer !== this.#peer) return;
				this.#capabilities = capabilities;
				this.schedule();
			},
			track: (mid) => {
				if (peer !== this.#peer) return;
				for (const binding of this.#bindings.values()) {
					if (binding.mid === mid)
						binding.state = { status: 'live', stream: peer.stream(mid), message: null };
				}
				this.emit();
			},
			failure: (error) => {
				if (peer === this.#peer) void this.recover(error);
			}
		});
		this.#peer = peer;
		this.#capabilities = await peer.open();
		if (peer !== this.#peer || this.#closed) return;
		this.#status = 'ready';
		this.#message = null;
	}

	private async synchronizeSource(peer: PeerAdapter, source: CardSource): Promise<void> {
		const key = sourceKey(source);
		const live = this.#capabilities?.sourceSessions.find(
			(session) => session.sourceId === source.source_id && session.video
		);
		const signature = JSON.stringify([
			live?.sourceSessionId,
			live?.video?.variants.map((variant) => variant.variantId)
		]);
		const previous = this.#bindings.get(key);
		if (previous?.signature === signature) return;
		if (previous?.subscriptionId) await peer.unsubscribe(previous.subscriptionId);
		if (peer !== this.#peer) return;
		const known = this.#capabilities?.cameras.some(
			(camera) => camera.sourceId === source.source_id
		);
		const binding: Binding = {
			signature,
			subscriptionId: null,
			mid: null,
			state: {
				status: 'unavailable',
				stream: null,
				message: known
					? 'Camera offline. Check its device connection.'
					: 'Unknown source ID. Choose a source listed by this server.'
			}
		};
		this.#bindings.set(key, binding);
		if (!live) return;
		binding.subscriptionId = `ha-${++this.#sequence}`;
		binding.state = { status: 'connecting', stream: null, message: null };
		try {
			const result = await peer.subscribe({
				subscriptionId: binding.subscriptionId,
				sourceSessionId: live.sourceSessionId,
				quality: source.quality
			});
			if (peer !== this.#peer) return;
			binding.mid = result.mid;
			binding.state = { status: 'live', stream: peer.stream(result.mid), message: null };
		} catch (error) {
			if (peer !== this.#peer) return;
			if (!(error instanceof CardSubscriptionError)) throw error;
			binding.subscriptionId = null;
			binding.state = { status: 'unavailable', stream: null, message: error.message };
		}
	}

	private recover(error: unknown): Promise<void> {
		if (this.#closed) return Promise.resolve();
		if (this.#recovering) return this.#recovering;
		clearTimeout(this.#timer);
		this.#timer = undefined;
		const failure = connectionError(error);
		this.#terminal = failure.kind === 'authentication' || this.#attempts >= 8;
		this.#status = this.#terminal ? 'error' : 'reconnecting';
		this.#message = failure.message;
		const peer = this.#peer;
		this.#peer = null;
		this.#bindings.clear();
		this.#capabilities = null;
		this.emit();
		const delayMs = this.#retryImmediately ? 0 : Math.min(1000 * 2 ** this.#attempts++, 30_000);
		this.#recovering = (peer?.close() ?? Promise.resolve())
			.catch((closeError: unknown) => {
				this.#terminal = true;
				this.#status = 'error';
				this.#message = connectionError(closeError).message;
				this.emit();
			})
			.then(() => {
				if (this.#closed || this.#terminal) return;
				const retryDelayMs = this.#retryImmediately ? 0 : delayMs;
				this.#retryImmediately = false;
				this.#timer = setTimeout(() => {
					this.#timer = undefined;
					this.schedule();
				}, retryDelayMs);
			})
			.finally(() => {
				this.#recovering = null;
			});
		return this.#recovering;
	}

	retry(): void {
		this.#attempts = 0;
		this.#terminal = false;
		this.#retryImmediately = true;
		void this.recover(new CardConnectionError('network', 'Reconnecting to KeepPeek.'));
	}

	async close(): Promise<void> {
		if (this.#closed) return;
		this.#closed = true;
		clearTimeout(this.#timer);
		const peer = this.#peer;
		this.#peer = null;
		this.#bindings.clear();
		this.#status = 'closed';
		await Promise.all([peer?.close(), this.#recovering]);
	}

	private emit(): void {
		if (this.#closed) return;
		const cameras = new Map<string, SourceChoice>();
		for (const camera of this.#capabilities?.cameras ?? []) {
			cameras.set(camera.sourceId, {
				source_id: camera.sourceId,
				title: camera.displayName || camera.sourceId,
				available: false
			});
		}
		for (const session of this.#capabilities?.sourceSessions ?? []) {
			if (session.sourceId && session.video)
				cameras.set(session.sourceId, {
					source_id: session.sourceId,
					title: cameras.get(session.sourceId)?.title ?? (session.displayName || session.sourceId),
					available: true
				});
		}
		this.#notify({
			status: this.#status,
			message: this.#message,
			cameras: [...cameras.values()],
			streams: new Map([...this.#bindings].map(([key, binding]) => [key, binding.state]))
		});
	}
}
