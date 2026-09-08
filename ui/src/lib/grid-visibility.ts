export type GridTileVisibility = {
	cameraId: string;
	visibleFraction: number;
	distanceFromViewportPx: number;
	viewportExtentPx: number;
};

type GridRect = Pick<DOMRectReadOnly, 'top' | 'right' | 'bottom' | 'left' | 'width' | 'height'>;

export function observeGridVisibility(
	node: Element,
	cameraId: string,
	onchange: (visibility: GridTileVisibility) => void
): () => void {
	const report = (rect: DOMRectReadOnly, intersection?: DOMRectReadOnly) => {
		onchange(
			measureGridVisibility(cameraId, rect, window.innerWidth, window.innerHeight, intersection)
		);
	};
	if (typeof IntersectionObserver === 'undefined') {
		report(node.getBoundingClientRect());
		return () => undefined;
	}
	const observe: IntersectionObserverCallback = (entries) => {
		const entry = entries[0];
		if (!entry) return;
		if (entry.boundingClientRect) {
			report(entry.boundingClientRect, entry.intersectionRect);
			return;
		}
		onchange({
			cameraId,
			visibleFraction: entry.intersectionRatio ?? (entry.isIntersecting ? 1 : 0),
			distanceFromViewportPx: entry.isIntersecting ? 0 : Number.POSITIVE_INFINITY,
			viewportExtentPx: Math.max(1, Math.max(window.innerWidth, window.innerHeight))
		});
	};
	const observers = ['0px', '100% 100%'].map(
		(rootMargin) =>
			new IntersectionObserver(observe, {
				root: null,
				rootMargin,
				threshold: [0, 0.01, 1 / 3, 2 / 3, 1]
			})
	);
	for (const observer of observers) observer.observe(node);
	return () => {
		for (const observer of observers) observer.disconnect();
	};
}

export function measureGridVisibility(
	cameraId: string,
	rect: GridRect,
	viewportWidth: number,
	viewportHeight: number,
	intersection: GridRect = rect
): GridTileVisibility {
	const visibleWidth = Math.max(
		0,
		Math.min(intersection.right, viewportWidth) - Math.max(intersection.left, 0)
	);
	const visibleHeight = Math.max(
		0,
		Math.min(intersection.bottom, viewportHeight) - Math.max(intersection.top, 0)
	);
	const area = Math.max(1, rect.width * rect.height);
	const horizontalDistance =
		rect.right < 0 ? -rect.right : rect.left > viewportWidth ? rect.left - viewportWidth : 0;
	const verticalDistance =
		rect.bottom < 0 ? -rect.bottom : rect.top > viewportHeight ? rect.top - viewportHeight : 0;
	return {
		cameraId,
		visibleFraction: Math.max(0, Math.min(1, (visibleWidth * visibleHeight) / area)),
		distanceFromViewportPx: Math.hypot(horizontalDistance, verticalDistance),
		viewportExtentPx: Math.max(1, Math.max(viewportWidth, viewportHeight))
	};
}
