const PAN_HOLD_MS = 350;
const PAN_CLICK_SUPPRESS_MS = 250;

export class TimelinePan {
	active = $state(false);

	#target: HTMLElement | null = null;
	#pointerId: number | null = null;
	#holdTimer: number | null = null;
	#frame: number | null = null;
	#startClientX = 0;
	#startClientY = 0;
	#startScrollLeft = 0;
	#startScrollTop = 0;
	#nextScrollLeft = 0;
	#nextScrollTop = 0;
	#suppressClick = false;

	get cursorClass(): string {
		return this.active ? 'cursor-grabbing' : 'cursor-crosshair';
	}

	begin(event: PointerEvent, target: HTMLElement): void {
		if (event.button !== 0 || this.#pointerId !== null) return;
		this.#target = target;
		this.#pointerId = event.pointerId;
		this.#startClientX = event.clientX;
		this.#startClientY = event.clientY;
		this.#startScrollLeft = target.scrollLeft;
		this.#startScrollTop = target.scrollTop;
		this.#nextScrollLeft = target.scrollLeft;
		this.#nextScrollTop = target.scrollTop;
		this.#holdTimer = window.setTimeout(() => {
			if (this.#pointerId !== event.pointerId || this.#target !== target) return;
			this.active = true;
			target.setPointerCapture(event.pointerId);
		}, PAN_HOLD_MS);
	}

	move(event: PointerEvent): void {
		if (event.pointerId !== this.#pointerId || !this.active || !this.#target) return;
		event.preventDefault();
		this.#nextScrollLeft = this.#startScrollLeft - (event.clientX - this.#startClientX);
		this.#nextScrollTop = this.#startScrollTop - (event.clientY - this.#startClientY);
		this.#scheduleScroll();
	}

	end(event: PointerEvent): boolean {
		if (event.pointerId !== this.#pointerId) return false;
		const panned = this.active;
		this.#clearHold();
		if (panned) {
			event.preventDefault();
			this.#applyScroll();
			this.#suppressClick = true;
			window.setTimeout(() => {
				this.#suppressClick = false;
			}, PAN_CLICK_SUPPRESS_MS);
			if (this.#target?.hasPointerCapture(event.pointerId)) {
				this.#target.releasePointerCapture(event.pointerId);
			}
		}
		this.#reset();
		return panned;
	}

	cancel(event: PointerEvent): void {
		if (event.pointerId !== this.#pointerId) return;
		this.#clearHold();
		if (this.#target?.hasPointerCapture(event.pointerId)) {
			this.#target.releasePointerCapture(event.pointerId);
		}
		this.#reset();
	}

	consumeClick(event: MouseEvent): void {
		if (!this.#suppressClick) return;
		this.#suppressClick = false;
		event.preventDefault();
		event.stopPropagation();
	}

	#scheduleScroll(): void {
		if (this.#frame !== null) return;
		this.#frame = requestAnimationFrame(() => this.#applyScroll());
	}

	#applyScroll(): void {
		if (this.#frame !== null) {
			cancelAnimationFrame(this.#frame);
			this.#frame = null;
		}
		if (!this.#target) return;
		this.#target.scrollLeft = this.#nextScrollLeft;
		this.#target.scrollTop = this.#nextScrollTop;
	}

	#clearHold(): void {
		if (this.#holdTimer === null) return;
		window.clearTimeout(this.#holdTimer);
		this.#holdTimer = null;
	}

	#reset(): void {
		this.active = false;
		this.#target = null;
		this.#pointerId = null;
	}
}
