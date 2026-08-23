export type GridTileVisibility = {
	cameraId: string;
	visibleFraction: number;
	distanceFromViewportPx: number;
	viewportExtentPx: number;
};

export function observeGridVisibility(
	node: Element,
	cameraId: string,
	onchange: (visibility: GridTileVisibility) => void
): () => void {
	const report = (rect: DOMRectReadOnly) => {
		onchange(measureGridVisibility(cameraId, rect, window.innerWidth, window.innerHeight));
	};
	if (typeof IntersectionObserver === 'undefined') {
		report(node.getBoundingClientRect());
		return () => undefined;
	}
	const observer = new IntersectionObserver(
		(entries) => {
			const entry = entries[0];
			if (!entry) return;
			if (entry.boundingClientRect) {
				report(entry.boundingClientRect);
				return;
			}
			onchange({
				cameraId,
				visibleFraction: entry.intersectionRatio ?? (entry.isIntersecting ? 1 : 0),
				distanceFromViewportPx: entry.isIntersecting ? 0 : Number.POSITIVE_INFINITY,
				viewportExtentPx: Math.max(1, Math.max(window.innerWidth, window.innerHeight))
			});
		},
		{
			root: null,
			rootMargin: '100% 100%',
			threshold: [0, 0.01, 1 / 3, 2 / 3, 1]
		}
	);
	observer.observe(node);
	return () => observer.disconnect();
}

export function measureGridVisibility(
	cameraId: string,
	rect: Pick<DOMRectReadOnly, 'top' | 'right' | 'bottom' | 'left' | 'width' | 'height'>,
	viewportWidth: number,
	viewportHeight: number
): GridTileVisibility {
	const visibleWidth = Math.max(0, Math.min(rect.right, viewportWidth) - Math.max(rect.left, 0));
	const visibleHeight = Math.max(0, Math.min(rect.bottom, viewportHeight) - Math.max(rect.top, 0));
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
