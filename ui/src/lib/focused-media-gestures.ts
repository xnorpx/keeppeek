import {
	clampMediaTransform,
	DIGITAL_ZOOM_INSPECTION,
	DIGITAL_ZOOM_MIN,
	pinchMediaTransform,
	resetMediaTransform,
	resizeMediaTransform,
	zoomMediaAt,
	type MediaPoint,
	type MediaTransform
} from './focused-media-geometry';

type MediaPointer = {
	point: MediaPoint;
	origin: MediaPoint;
	startedAtMs: number;
	moved: boolean;
	pointerType: string;
};

export function createFocusedMediaControls(
	viewport: HTMLElement,
	layer: HTMLElement,
	onscalechange: (scale: number) => void
) {
	return new FocusedMediaControls(viewport, layer, onscalechange);
}

class FocusedMediaControls {
	private readonly listeners = new AbortController();
	private readonly viewport: HTMLElement;
	private readonly layer: HTMLElement;
	private readonly onscalechange: (scale: number) => void;
	private readonly originalViewportStyles;
	private readonly originalLayerStyles;
	private bounds: DOMRect;
	private transform = resetMediaTransform();
	private reportedScale: number | null = null;
	private frame: number | null = null;
	private readonly pointers = new Map<number, MediaPointer>();
	private hadMultiplePointers = false;
	private lastTap: { point: MediaPoint; timeMs: number } | null = null;
	private lastDoubleTapMs = Number.NEGATIVE_INFINITY;
	private destroyed = false;

	constructor(viewport: HTMLElement, layer: HTMLElement, onscalechange: (scale: number) => void) {
		this.viewport = viewport;
		this.layer = layer;
		this.onscalechange = onscalechange;
		this.bounds = viewport.getBoundingClientRect();
		this.originalViewportStyles = {
			overflow: viewport.style.overflow,
			touchAction: viewport.style.touchAction,
			userSelect: viewport.style.userSelect,
			cursor: viewport.style.cursor
		};
		this.originalLayerStyles = {
			transform: layer.style.transform,
			transformOrigin: layer.style.transformOrigin,
			willChange: layer.style.willChange
		};
		Object.assign(viewport.style, { overflow: 'hidden', touchAction: 'none', userSelect: 'none' });
		Object.assign(layer.style, { transformOrigin: 'center', willChange: 'transform' });
		const options = { signal: this.listeners.signal };
		viewport.addEventListener('wheel', this.handleWheel, { ...options, passive: false });
		viewport.addEventListener('dblclick', this.handleDoubleClick, options);
		viewport.addEventListener('keydown', this.handleKeydown, options);
		viewport.addEventListener('pointerdown', this.handlePointerDown, options);
		viewport.addEventListener('pointermove', this.handlePointerMove, options);
		viewport.addEventListener('pointerup', this.handlePointerUp, options);
		viewport.addEventListener('pointercancel', this.handlePointerCancel, options);
		viewport.addEventListener('lostpointercapture', this.handlePointerCancel, options);
		window.addEventListener('blur', this.cancelPointers, options);
	}

	zoomIn = () => this.zoomAt(this.transform.scale * 2, { x: 0, y: 0 });
	zoomOut = () => this.zoomAt(this.transform.scale / 2, { x: 0, y: 0 });
	focus = () => this.viewport.focus({ preventScroll: true });
	reset = () => {
		this.cancelPointers();
		this.update(resetMediaTransform());
	};

	refresh = () => {
		this.cancelPointers();
		const bounds = this.viewport.getBoundingClientRect();
		this.update(resizeMediaTransform(this.transform, this.bounds, bounds));
		this.bounds = bounds;
	};

	destroy = () => {
		if (this.destroyed) return;
		this.destroyed = true;
		this.listeners.abort();
		this.cancelPointers();
		if (this.frame !== null) cancelAnimationFrame(this.frame);
		this.viewport.removeAttribute('data-digital-pan-active');
		Object.assign(this.viewport.style, this.originalViewportStyles);
		Object.assign(this.layer.style, this.originalLayerStyles);
	};

	private update(transform: MediaTransform) {
		if (this.destroyed) return;
		if (transform.scale > DIGITAL_ZOOM_MIN !== this.transform.scale > DIGITAL_ZOOM_MIN) {
			this.viewport.toggleAttribute('data-digital-pan-active', transform.scale > DIGITAL_ZOOM_MIN);
		}
		this.transform = transform;
		if (this.frame !== null) return;
		this.frame = requestAnimationFrame(() => {
			this.frame = null;
			const { scale, panX, panY } = this.transform;
			this.layer.style.transform = `translate(${panX}px, ${panY}px) scale(${scale})`;
			this.viewport.style.cursor = scale > DIGITAL_ZOOM_MIN ? 'grab' : 'default';
			if (scale === this.reportedScale) return;
			this.reportedScale = scale;
			this.onscalechange(scale);
		});
	}

	private zoomAt(scale: number, point: MediaPoint) {
		this.update(zoomMediaAt(this.transform, scale, point, this.bounds));
	}

	private localPoint(event: { clientX: number; clientY: number }): MediaPoint {
		return {
			x: event.clientX - this.bounds.left - this.bounds.width / 2,
			y: event.clientY - this.bounds.top - this.bounds.height / 2
		};
	}

	private handleWheel = (event: WheelEvent) => {
		if (!event.altKey || event.ctrlKey || event.metaKey) return;
		const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? this.bounds.height : 1;
		const delta = Math.max(-240, Math.min(240, (event.deltaY || event.deltaX) * unit));
		if (!Number.isFinite(delta)) return;
		event.preventDefault();
		this.bounds = this.viewport.getBoundingClientRect();
		this.zoomAt(this.transform.scale * Math.exp(-delta * 0.002), this.localPoint(event));
	};

	private handleDoubleClick = (event: MouseEvent) => {
		if (event.ctrlKey || event.metaKey) return;
		event.preventDefault();
		event.stopPropagation();
		if (event.timeStamp - this.lastDoubleTapMs < 400) return;
		this.bounds = this.viewport.getBoundingClientRect();
		if (this.transform.scale > DIGITAL_ZOOM_MIN) this.reset();
		else this.zoomAt(DIGITAL_ZOOM_INSPECTION, this.localPoint(event));
	};

	private handlePointerDown = (event: PointerEvent) => {
		if (event.button !== 0 || event.ctrlKey || event.metaKey || this.pointers.size >= 2) return;
		if (this.pointers.has(event.pointerId)) return;
		this.bounds = this.viewport.getBoundingClientRect();
		const point = this.localPoint(event);
		if (this.pointers.size === 0) this.hadMultiplePointers = false;
		this.pointers.set(event.pointerId, {
			point,
			origin: point,
			startedAtMs: event.timeStamp,
			moved: false,
			pointerType: event.pointerType
		});
		if (this.pointers.size === 2) {
			this.hadMultiplePointers = true;
			this.lastTap = null;
		}
		if (event.isTrusted) this.viewport.setPointerCapture(event.pointerId);
		this.viewport.focus({ preventScroll: true });
		event.preventDefault();
	};

	private handlePointerMove = (event: PointerEvent) => {
		const pointer = this.pointers.get(event.pointerId);
		if (!pointer) return;
		const point = this.localPoint(event);
		pointer.moved ||= Math.hypot(point.x - pointer.origin.x, point.y - pointer.origin.y) > 8;
		const [first, second] = this.pointers.values();
		if (second) {
			const previous: [MediaPoint, MediaPoint] = [first.point, second.point];
			pointer.point = point;
			this.update(
				pinchMediaTransform(this.transform, previous, [first.point, second.point], this.bounds)
			);
		} else if (this.transform.scale > DIGITAL_ZOOM_MIN) {
			this.panBy({ x: point.x - pointer.point.x, y: point.y - pointer.point.y });
		}
		pointer.point = point;
		event.preventDefault();
	};

	private handlePointerUp = (event: PointerEvent) => {
		const pointer = this.pointers.get(event.pointerId);
		if (!pointer) return;
		this.handlePointerMove(event);
		this.pointers.delete(event.pointerId);
		if (this.viewport.hasPointerCapture(event.pointerId))
			this.viewport.releasePointerCapture(event.pointerId);
		if (
			pointer.pointerType !== 'touch' ||
			pointer.moved ||
			this.hadMultiplePointers ||
			event.timeStamp - pointer.startedAtMs > 300
		) {
			this.lastTap = null;
			return;
		}
		const previous = this.lastTap;
		const point = pointer.point;
		if (
			previous &&
			event.timeStamp - previous.timeMs <= 300 &&
			Math.hypot(point.x - previous.point.x, point.y - previous.point.y) <= 24
		) {
			if (this.transform.scale > DIGITAL_ZOOM_MIN) this.reset();
			else this.zoomAt(DIGITAL_ZOOM_INSPECTION, point);
			this.lastTap = null;
			this.lastDoubleTapMs = event.timeStamp;
		} else {
			this.lastTap = { point, timeMs: event.timeStamp };
		}
	};

	private handlePointerCancel = (event: PointerEvent) => {
		if (this.pointers.has(event.pointerId)) this.cancelPointers();
	};

	private cancelPointers = () => {
		const pointerIds = [...this.pointers.keys()];
		this.pointers.clear();
		this.lastTap = null;
		for (const pointerId of pointerIds) {
			if (this.viewport.hasPointerCapture(pointerId))
				this.viewport.releasePointerCapture(pointerId);
		}
	};

	private panBy(delta: MediaPoint) {
		this.update(
			clampMediaTransform(
				{
					...this.transform,
					panX: this.transform.panX + delta.x,
					panY: this.transform.panY + delta.y
				},
				this.bounds
			)
		);
	}

	private handleKeydown = (event: KeyboardEvent) => {
		if (event.ctrlKey || event.metaKey || event.altKey) return;
		if (
			event.target instanceof Element &&
			event.target.closest('button, input, select, textarea, a, [contenteditable="true"]')
		)
			return;
		switch (event.key) {
			case '+':
			case '=':
				this.zoomIn();
				break;
			case '-':
				this.zoomOut();
				break;
			case '0':
				this.reset();
				break;
			case 'Escape':
				if (this.transform.scale === DIGITAL_ZOOM_MIN) return;
				this.reset();
				break;
			case 'ArrowLeft':
			case 'ArrowRight':
			case 'ArrowUp':
			case 'ArrowDown': {
				if (this.transform.scale === DIGITAL_ZOOM_MIN) return;
				const horizontal = event.key === 'ArrowRight' ? 40 : event.key === 'ArrowLeft' ? -40 : 0;
				const vertical = event.key === 'ArrowDown' ? 40 : event.key === 'ArrowUp' ? -40 : 0;
				this.panBy({ x: horizontal, y: vertical });
				break;
			}
			default:
				return;
		}
		event.preventDefault();
		event.stopPropagation();
	};
}
