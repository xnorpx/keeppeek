import { mount, unmount } from 'svelte';
import Card from './Card.svelte';
import Editor from './Editor.svelte';
import styles from './card.css?inline';
import {
	configObject,
	maxSources,
	parseCardConfig,
	type CardConfig,
	type CardSource
} from './config';
import {
	KeepPeekConnectionManager,
	type CardLease,
	type SessionSnapshot,
	type SourceChoice
} from './connection-manager';
import { DirectSession } from './direct-session';

export const version: string = import.meta.env.VITE_KEEPPEEK_CARD_VERSION ?? 'development';

export class CardView {
	config = $state.raw<CardConfig | null>(null);
	snapshot = $state.raw<SessionSnapshot | null>(null);
	visible = $state(false);
}

class EditorView {
	config = $state.raw<Record<string, unknown>>({});
	cameras = $state.raw<readonly SourceChoice[]>([]);
	message = $state<string | null>(null);
}

function manager(): KeepPeekConnectionManager {
	const key = Symbol.for('keeppeek.card.connections.v1');
	const existing: unknown = Reflect.get(window, key);
	if (existing instanceof KeepPeekConnectionManager) return existing;
	const created = new KeepPeekConnectionManager(
		(config, notify) => new DirectSession(config, notify)
	);
	Reflect.set(window, key, created);
	return created;
}

function shadow(element: HTMLElement): ShadowRoot {
	const root = element.attachShadow({ mode: 'open' });
	const style = document.createElement('style');
	style.textContent = styles;
	root.append(style);
	return root;
}

if (typeof window !== 'undefined') {
	class KeepPeekCard extends HTMLElement {
		hass: unknown;
		#root = shadow(this);
		#view = new CardView();
		#mounted: ReturnType<typeof mount> | null = null;
		#events: AbortController | null = null;
		#observer: IntersectionObserver | null = null;
		#intersecting = false;
		#acquisition: AbortController | null = null;
		#lease: CardLease | null = null;
		#sources: CardSource[] = [];

		setConfig(value: unknown): void {
			let config: CardConfig;
			try {
				config = parseCardConfig(value);
			} catch (error) {
				this.release();
				this.#view.config = null;
				this.#sources = [];
				this.report('Invalid card configuration. Check the endpoint, access key, and sources.');
				throw error;
			}
			if (
				config.endpoint !== this.#view.config?.endpoint ||
				config.token !== this.#view.config?.token
			)
				this.release();
			this.#view.config = config;
			this.demand(config.layout === 'single' ? config.sources.slice(0, 1) : config.sources);
			this.activity();
		}

		connectedCallback(): void {
			if (this.#mounted) return;
			this.#mounted = mount(Card, {
				target: this.#root,
				props: {
					state: this.#view,
					onretry: () => {
						if (this.#lease) this.#lease.retry();
						else {
							this.release();
							this.activity();
						}
					},
					ondemand: (sources: CardSource[]) => this.demand(sources)
				}
			});
			this.#events = new AbortController();
			const options = { signal: this.#events.signal };
			document.addEventListener('visibilitychange', () => this.activity(), options);
			window.addEventListener('pagehide', () => this.release(), options);
			window.addEventListener('pageshow', () => this.activity(), options);
			this.#observer = new IntersectionObserver((entries) => {
				this.#intersecting = entries.some((entry) => entry.isIntersecting);
				this.activity();
			});
			this.#observer.observe(this);
		}

		disconnectedCallback(): void {
			queueMicrotask(() => {
				if (this.isConnected) return;
				this.#events?.abort();
				this.#observer?.disconnect();
				this.#intersecting = false;
				this.release();
				if (this.#mounted) void unmount(this.#mounted);
				this.#mounted = null;
			});
		}

		private demand(sources: CardSource[]): void {
			this.#sources = sources;
			try {
				this.#lease?.updateSources(sources);
			} catch {
				this.release();
				this.report('Limit this server to 16 simultaneous source and quality selections.');
			}
		}

		private activity(): void {
			this.#view.visible =
				this.isConnected && this.#intersecting && document.visibilityState !== 'hidden';
			if (!this.#view.visible) {
				this.release();
				return;
			}
			const config = this.#view.config;
			if (!config || this.#acquisition) return;
			const controller = new AbortController();
			this.#acquisition = controller;
			this.#view.snapshot = null;
			void manager()
				.acquire(
					{ ...config, sources: this.#sources },
					(snapshot) => {
						if (!controller.signal.aborted) this.#view.snapshot = snapshot;
					},
					controller.signal
				)
				.then(async (lease) => {
					if (controller.signal.aborted) {
						await lease.release();
						return;
					}
					this.#lease = lease;
					lease.updateSources(this.#sources);
				})
				.catch(() => {
					if (!controller.signal.aborted) {
						this.release();
						this.report(
							'Cannot open this card. Check its settings and the shared connection limits.'
						);
					}
				});
		}

		private release(): void {
			this.#acquisition?.abort();
			this.#acquisition = null;
			const lease = this.#lease;
			this.#lease = null;
			if (lease)
				void lease
					.release()
					.catch(() =>
						this.report('The session could not be deleted. Check KeepPeek connectivity.')
					);
		}

		private report(message: string): void {
			this.#view.snapshot = { status: 'error', message, cameras: [], streams: new Map() };
		}

		getCardSize(): number {
			return Math.max(3, Math.ceil(this.getBoundingClientRect().height / 50));
		}
		getGridOptions() {
			return { columns: 12, min_columns: 3, min_rows: 3 };
		}
		static getConfigElement(): HTMLElement {
			return document.createElement('keeppeek-card-editor');
		}
		static getStubConfig() {
			return { endpoint: '', token: '', sources: [], layout: 'grid', columns: 2 };
		}
	}

	class KeepPeekCardEditor extends HTMLElement {
		hass: unknown;
		#root = shadow(this);
		#view = new EditorView();
		#mounted: ReturnType<typeof mount> | null = null;
		#acquisition: AbortController | null = null;
		#lease: CardLease | null = null;

		setConfig(value: unknown): void {
			const config = configObject(value);
			if (
				Array.isArray(config.sources) &&
				(config.sources.length > maxSources ||
					config.sources.some((source) => !source || typeof source !== 'object'))
			)
				throw new Error('The editor supports at most 16 source objects.');
			if (
				config.endpoint !== this.#view.config.endpoint ||
				config.token !== this.#view.config.token
			)
				this.release();
			this.#view.config = { ...config };
		}

		connectedCallback(): void {
			if (this.#mounted) return;
			const state = this.#view;
			this.#mounted = mount(Editor, {
				target: this.#root,
				props: {
					get config() {
						return state.config;
					},
					get cameras() {
						return state.cameras;
					},
					get message() {
						return state.message;
					},
					onchange: (config: Record<string, unknown>) => {
						this.setConfig(config);
						this.dispatchEvent(
							new CustomEvent('config-changed', {
								detail: { config },
								bubbles: true,
								composed: true
							})
						);
					},
					ondiscover: () => {
						void this.discover();
					}
				}
			});
		}

		disconnectedCallback(): void {
			queueMicrotask(() => {
				if (this.isConnected) return;
				this.release();
				if (this.#mounted) void unmount(this.#mounted);
				this.#mounted = null;
			});
		}

		private async discover(): Promise<void> {
			this.release();
			let config: CardConfig;
			try {
				config = parseCardConfig({
					...this.#view.config,
					type: 'custom:keeppeek-card',
					sources: [{ source_id: 'discovery' }]
				});
			} catch (error) {
				this.#view.message =
					error instanceof Error ? error.message : 'Check the card configuration.';
				return;
			}
			const controller = new AbortController();
			this.#acquisition = controller;
			try {
				const lease = await manager().acquire(
					{ ...config, sources: [] },
					(snapshot) => {
						if (controller.signal.aborted) return;
						this.#view.cameras = snapshot.cameras;
						this.#view.message = snapshot.message;
					},
					controller.signal
				);
				if (controller.signal.aborted) await lease.release();
				else this.#lease = lease;
			} catch {
				if (!controller.signal.aborted)
					this.#view.message = 'Cannot load sources. Check the connection settings and limits.';
			}
		}

		private release(): void {
			this.#acquisition?.abort();
			this.#acquisition = null;
			const lease = this.#lease;
			this.#lease = null;
			if (lease)
				void lease.release().catch(() => {
					this.#view.message = 'The discovery session could not be deleted.';
				});
		}
	}

	if (!customElements.get('keeppeek-card')) customElements.define('keeppeek-card', KeepPeekCard);
	if (!customElements.get('keeppeek-card-editor'))
		customElements.define('keeppeek-card-editor', KeepPeekCardEditor);
	const cards: Array<{ type: string; name: string; documentationURL?: string }> =
		Reflect.get(window, 'customCards') ?? [];
	if (!cards.some((card) => card.type === 'keeppeek-card'))
		cards.push({
			type: 'keeppeek-card',
			name: 'KeepPeek',
			documentationURL: 'https://github.com/xnorpx/keeppeek/blob/main/docs/home-assistant.md'
		});
	Reflect.set(window, 'customCards', cards);
}
