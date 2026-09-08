export type MediaSize = { width: number; height: number };
export type MediaPoint = { x: number; y: number };
export type MediaTransform = { scale: number; panX: number; panY: number };

export const DIGITAL_ZOOM_MIN = 1;
export const DIGITAL_ZOOM_MAX = 8;
export const DIGITAL_ZOOM_INSPECTION = 2;

export function resetMediaTransform(): MediaTransform {
	return { scale: DIGITAL_ZOOM_MIN, panX: 0, panY: 0 };
}

export function clampMediaTransform(transform: MediaTransform, bounds: MediaSize): MediaTransform {
	if (!hasDimensions(bounds) || !Number.isFinite(transform.scale)) return resetMediaTransform();
	const scale = Math.min(DIGITAL_ZOOM_MAX, Math.max(DIGITAL_ZOOM_MIN, transform.scale));
	const horizontalLimit = (bounds.width * (scale - 1)) / 2;
	const verticalLimit = (bounds.height * (scale - 1)) / 2;
	const panX = Number.isFinite(transform.panX) ? transform.panX : 0;
	const panY = Number.isFinite(transform.panY) ? transform.panY : 0;
	return {
		scale,
		panX: Math.min(horizontalLimit, Math.max(-horizontalLimit, panX)),
		panY: Math.min(verticalLimit, Math.max(-verticalLimit, panY))
	};
}

export function zoomMediaAt(
	transform: MediaTransform,
	requestedScale: number,
	point: MediaPoint,
	bounds: MediaSize
): MediaTransform {
	return transformMediaBetween(transform, requestedScale, point, point, bounds);
}

export function pinchMediaTransform(
	transform: MediaTransform,
	previous: readonly [MediaPoint, MediaPoint],
	current: readonly [MediaPoint, MediaPoint],
	bounds: MediaSize
): MediaTransform {
	const previousDistance = Math.hypot(previous[1].x - previous[0].x, previous[1].y - previous[0].y);
	const currentDistance = Math.hypot(current[1].x - current[0].x, current[1].y - current[0].y);
	if (
		previousDistance < 1 ||
		!Number.isFinite(previousDistance) ||
		!Number.isFinite(currentDistance)
	) {
		return clampMediaTransform(transform, bounds);
	}
	return transformMediaBetween(
		transform,
		(transform.scale * currentDistance) / previousDistance,
		{ x: (previous[0].x + previous[1].x) / 2, y: (previous[0].y + previous[1].y) / 2 },
		{ x: (current[0].x + current[1].x) / 2, y: (current[0].y + current[1].y) / 2 },
		bounds
	);
}

export function resizeMediaTransform(
	transform: MediaTransform,
	previous: MediaSize,
	current: MediaSize
): MediaTransform {
	if (!hasDimensions(previous)) return clampMediaTransform(transform, current);
	return clampMediaTransform(
		{
			scale: transform.scale,
			panX: (transform.panX * current.width) / previous.width,
			panY: (transform.panY * current.height) / previous.height
		},
		current
	);
}

function transformMediaBetween(
	transform: MediaTransform,
	requestedScale: number,
	origin: MediaPoint,
	target: MediaPoint,
	bounds: MediaSize
): MediaTransform {
	const current = clampMediaTransform(transform, bounds);
	if (![origin.x, origin.y, target.x, target.y].every(Number.isFinite)) return current;
	const bounded = clampMediaTransform({ ...current, scale: requestedScale }, bounds);
	const ratio = bounded.scale / current.scale;
	return clampMediaTransform(
		{
			scale: bounded.scale,
			panX: target.x - (origin.x - current.panX) * ratio,
			panY: target.y - (origin.y - current.panY) * ratio
		},
		bounds
	);
}

function hasDimensions(size: MediaSize): boolean {
	return (
		Number.isFinite(size.width) && Number.isFinite(size.height) && size.width > 0 && size.height > 0
	);
}

export function fitMediaSize(viewport: MediaSize, media: MediaSize): MediaSize {
	if (!hasDimensions(viewport)) return { width: 0, height: 0 };
	if (!hasDimensions(media)) return { ...viewport };
	const scale = Math.min(viewport.width / media.width, viewport.height / media.height);
	return { width: media.width * scale, height: media.height * scale };
}
