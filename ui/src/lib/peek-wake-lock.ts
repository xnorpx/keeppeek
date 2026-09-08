export type PeekWakeLockState =
	| 'off'
	| 'awaiting-gesture'
	| 'requesting'
	| 'active'
	| 'released'
	| 'unsupported'
	| 'denied'
	| 'release-failed';

export type ScreenWakeLockHandle = Pick<
	WakeLockSentinel,
	'released' | 'release' | 'addEventListener' | 'removeEventListener'
>;

export class PeekWakeLock {
	#enabled = false;
	#visible = true;
	#activated = false;
	#denied = false;
	#disposed = false;
	#pending = false;
	#generation = 0;
	#releasing: Promise<boolean> | null = null;
	#requestTimer: ReturnType<typeof setTimeout> | null = null;
	#sentinel: ScreenWakeLockHandle | null = null;
	#releaseListener: (() => void) | null = null;
	#request: (() => Promise<ScreenWakeLockHandle>) | null;
	#onstatechange: (state: PeekWakeLockState) => void;

	constructor(
		request: (() => Promise<ScreenWakeLockHandle>) | null,
		onstatechange: (state: PeekWakeLockState) => void
	) {
		this.#request = request;
		this.#onstatechange = onstatechange;
		onstatechange(request ? 'off' : 'unsupported');
	}

	async setEnabled(enabled: boolean, userGesture = false): Promise<void> {
		if (this.#disposed) return;
		if (this.#enabled !== enabled) {
			this.#invalidate();
			this.#denied = false;
		}
		this.#enabled = enabled;
		if (userGesture) this.#activated = true;
		if (enabled) return this.#acquire();
		if (await this.#release()) await this.#acquire();
	}

	async setVisible(visible: boolean): Promise<void> {
		if (this.#disposed || visible === this.#visible) return;
		this.#visible = visible;
		this.#invalidate();
		if (visible) return this.#acquire();
		if (await this.#release()) await this.#acquire();
	}

	async activate(): Promise<void> {
		if (this.#disposed) return;
		this.#activated = true;
		if (this.#enabled && !this.#denied) await this.#acquire();
	}

	async dispose(): Promise<void> {
		if (this.#disposed) return;
		this.#disposed = true;
		this.#enabled = false;
		this.#invalidate();
		if (await this.#release()) this.#onstatechange('off');
	}

	#invalidate(): void {
		this.#generation += 1;
		if (this.#requestTimer !== null) clearTimeout(this.#requestTimer);
		this.#requestTimer = null;
	}

	#reportIdle(): void {
		this.#onstatechange(
			!this.#request
				? 'unsupported'
				: !this.#enabled
					? 'off'
					: this.#denied
						? 'denied'
						: !this.#visible
							? 'released'
							: 'awaiting-gesture'
		);
	}

	async #acquire(): Promise<void> {
		if (this.#disposed) return;
		if (!this.#request || !this.#enabled || !this.#visible || !this.#activated || this.#denied) {
			this.#reportIdle();
			return;
		}
		if (this.#pending || this.#sentinel || this.#releasing) return;
		this.#pending = true;
		const generation = this.#generation;
		this.#onstatechange('requesting');
		const timer = setTimeout(() => {
			if (generation !== this.#generation || this.#disposed) return;
			this.#generation += 1;
			this.#denied = true;
			this.#onstatechange('denied');
		}, 10_000);
		this.#requestTimer = timer;
		try {
			const sentinel = await this.#request();
			if (generation !== this.#generation || this.#disposed) {
				await this.#releaseHandle(sentinel);
				return;
			}
			this.#retain(sentinel);
		} catch {
			if (generation === this.#generation && !this.#disposed) {
				this.#denied = true;
				this.#onstatechange('denied');
			}
		} finally {
			clearTimeout(timer);
			if (this.#requestTimer === timer) this.#requestTimer = null;
			this.#pending = false;
			if (
				generation !== this.#generation &&
				!this.#disposed &&
				this.#enabled &&
				this.#visible &&
				!this.#denied
			) {
				queueMicrotask(() => void this.#acquire());
			}
		}
	}

	#retain(sentinel: ScreenWakeLockHandle): void {
		if (sentinel.released) {
			this.#onstatechange('released');
			return;
		}
		this.#sentinel = sentinel;
		this.#releaseListener = () => {
			if (this.#sentinel !== sentinel) return;
			this.#detach(sentinel);
			if (!this.#disposed) this.#onstatechange(this.#enabled ? 'released' : 'off');
		};
		sentinel.addEventListener('release', this.#releaseListener, { once: true });
		this.#onstatechange('active');
	}

	#detach(sentinel: ScreenWakeLockHandle): void {
		if (this.#sentinel !== sentinel) return;
		if (this.#releaseListener) sentinel.removeEventListener('release', this.#releaseListener);
		this.#releaseListener = null;
		this.#sentinel = null;
	}

	async #release(): Promise<boolean> {
		if (this.#releasing) return this.#releasing;
		const sentinel = this.#sentinel;
		if (!sentinel) return true;
		const releasing = this.#releaseHandle(sentinel)
			.then((released) => {
				if (released) this.#detach(sentinel);
				return released;
			})
			.finally(() => {
				if (this.#releasing === releasing) this.#releasing = null;
			});
		this.#releasing = releasing;
		return releasing;
	}

	async #releaseHandle(sentinel: ScreenWakeLockHandle): Promise<boolean> {
		try {
			await sentinel.release();
			return true;
		} catch {
			this.#onstatechange('release-failed');
			return false;
		}
	}
}
