import { getContext, setContext } from 'svelte';
import type { ControlClient } from './control-client';
import type { PeekLayoutRegistry } from './peek-layout';
import type { CameraListItem, ServerHealthResponse } from './types';

const PEEK_VIEW_STATE_CONTEXT = Symbol('keeppeek.peek-view-state');

export type PeekTransition = {
	dataUrl: string;
	destination: 'dashboard' | 'viewer';
	cameraId: string | null;
};

export class PeekViewState {
	#cameras = $state.raw<CameraListItem[]>([]);
	#serverHealth = $state.raw<ServerHealthResponse | null>(null);
	#layoutRegistry = $state.raw<PeekLayoutRegistry | null>(null);
	#error = $state<string | null>(null);
	#layoutError = $state<string | null>(null);
	#loaded = $state(false);
	#wallRevealed = $state(false);
	#transition = $state.raw<PeekTransition | null>(null);
	#cameraFrames = $state.raw<Record<string, string>>({});
	#refresh: Promise<void> | null = null;
	#generation = 0;

	get cameras(): CameraListItem[] {
		return this.#cameras;
	}

	get serverHealth(): ServerHealthResponse | null {
		return this.#serverHealth;
	}

	get layoutRegistry(): PeekLayoutRegistry | null {
		return this.#layoutRegistry;
	}

	get error(): string | null {
		return this.#error;
	}

	get layoutError(): string | null {
		return this.#layoutError;
	}

	get loaded(): boolean {
		return this.#loaded;
	}

	get wallRevealed(): boolean {
		return this.#wallRevealed;
	}

	get transition(): PeekTransition | null {
		return this.#transition;
	}

	cameraFrame(cameraId: string): string | null {
		return this.#cameraFrames[cameraId] ?? null;
	}

	get generation(): number {
		return this.#generation;
	}

	refresh(controller: ControlClient): Promise<void> {
		if (this.#refresh) return this.#refresh;
		const refresh = this.#refreshNow(controller, this.#generation);
		this.#refresh = refresh;
		return refresh.finally(() => {
			if (this.#refresh === refresh) this.#refresh = null;
		});
	}

	reset(): void {
		this.#generation += 1;
		this.#refresh = null;
		this.#cameras = [];
		this.#serverHealth = null;
		this.#layoutRegistry = null;
		this.#error = null;
		this.#layoutError = null;
		this.#loaded = false;
		this.#wallRevealed = false;
		this.#transition = null;
		this.#cameraFrames = {};
	}

	updateCameraFrames(frames: Readonly<Record<string, string>>): void {
		this.#cameraFrames = { ...this.#cameraFrames, ...frames };
	}

	beginTransition(transition: PeekTransition): void {
		this.#transition = transition;
	}

	finishTransition(transition: PeekTransition): void {
		if (this.#transition === transition) this.#transition = null;
	}

	updateHealth(generation: number, health: ServerHealthResponse | null): boolean {
		if (generation !== this.#generation) return false;
		this.#serverHealth = health;
		return true;
	}

	updateLayoutRegistry(generation: number, registry: PeekLayoutRegistry): boolean {
		if (generation !== this.#generation) return false;
		this.#layoutRegistry = registry;
		return true;
	}

	updateLayoutError(generation: number, error: string | null): boolean {
		if (generation !== this.#generation) return false;
		this.#layoutError = error;
		return true;
	}

	updateFromSettings(
		generation: number,
		cameras: CameraListItem[],
		health: ServerHealthResponse | null,
		registry: PeekLayoutRegistry
	): boolean {
		if (generation !== this.#generation) return false;
		this.#cameras = cameras;
		this.#serverHealth = health;
		this.#layoutRegistry = registry;
		this.#error = null;
		this.#layoutError = null;
		this.#loaded = true;
		return true;
	}

	markWallRevealed(): void {
		this.#wallRevealed = true;
	}

	async #refreshNow(controller: ControlClient, generation: number): Promise<void> {
		const hadSnapshot = this.#loaded && this.#error === null;
		try {
			const [camerasResult, healthResult, capabilitiesResult] = await Promise.allSettled([
				controller.getCameras(),
				controller.getHealth(),
				controller.getServerCapabilities()
			]);
			if (generation !== this.#generation) return;
			if (camerasResult.status === 'rejected') throw camerasResult.reason;
			this.#cameras = camerasResult.value;
			if (healthResult.status === 'fulfilled') this.#serverHealth = healthResult.value;
			if (capabilitiesResult.status === 'fulfilled') {
				if (capabilitiesResult.value.capabilityIds.includes('keeppeek.peek-layouts.v1')) {
					try {
						const layoutRegistry = await controller.getPeekLayoutRegistry();
						if (generation !== this.#generation) return;
						this.#layoutRegistry = layoutRegistry;
						this.#layoutError = null;
					} catch (cause) {
						if (generation !== this.#generation) return;
						this.#layoutError =
							cause instanceof Error ? cause.message : 'Failed to load saved Peek layouts.';
					}
				} else {
					this.#layoutRegistry = null;
					this.#layoutError = null;
				}
			}
			this.#error = null;
		} catch (cause) {
			if (generation === this.#generation && !hadSnapshot) {
				this.#error = cause instanceof Error ? cause.message : 'Failed to load dashboard';
			}
		} finally {
			if (generation === this.#generation) this.#loaded = true;
		}
	}
}

export function setPeekViewState(): PeekViewState {
	return setContext(PEEK_VIEW_STATE_CONTEXT, new PeekViewState());
}

export function usePeekViewState(): PeekViewState {
	const state = getContext<PeekViewState | undefined>(PEEK_VIEW_STATE_CONTEXT);
	if (!state) throw new Error('Peek view state context is unavailable');
	return state;
}
