import type { GridStreamingMode } from './grid-stream-scheduler';

export const maximumWallStreams = 12;
export const maximumWallDecorationPx = 24;
export const peekWallAppearancePresets = [
	{ id: 'current', label: 'Current', gapPx: 10, cornerRadiusPx: 10 },
	{ id: 'hairline', label: 'Hairline', gapPx: 2, cornerRadiusPx: 0 },
	{ id: 'flush', label: 'Flush', gapPx: 0, cornerRadiusPx: 0 }
] as const;
export type PeekTileShape = '16:9' | '4:3' | 'native';
export type PeekMediaFit = 'contain' | 'cover';
export type PeekWallPreferences = {
	version: 1;
	tileShape: PeekTileShape;
	mediaFit: PeekMediaFit;
	streamingMode: GridStreamingMode;
	streamLimit: number;
	keepAwake: boolean;
	gapPx: number;
	cornerRadiusPx: number;
};

export function defaultPeekWallPreferences(): PeekWallPreferences {
	return {
		version: 1,
		tileShape: '16:9',
		mediaFit: 'contain',
		streamingMode: 'smart',
		streamLimit: maximumWallStreams,
		keepAwake: false,
		gapPx: 10,
		cornerRadiusPx: 10
	};
}

export function peekWallAppearancePreset(
	preferences: Pick<PeekWallPreferences, 'gapPx' | 'cornerRadiusPx'>
) {
	return (
		peekWallAppearancePresets.find(
			(preset) =>
				preset.gapPx === preferences.gapPx && preset.cornerRadiusPx === preferences.cornerRadiusPx
		)?.id ?? 'custom'
	);
}

export function wallDisplayToWire(preferences: PeekWallPreferences) {
	return {
		version: preferences.version,
		tile_shape: preferences.tileShape,
		media_fit: preferences.mediaFit,
		streaming_mode: preferences.streamingMode,
		stream_limit: preferences.streamLimit,
		keep_awake: preferences.keepAwake,
		gap_px: preferences.gapPx,
		corner_radius_px: preferences.cornerRadiusPx
	};
}

export function wallDisplayFromWire(value: unknown): PeekWallPreferences {
	if (typeof value !== 'object' || value === null || Array.isArray(value)) {
		throw new Error('Dashboard display settings are invalid.');
	}
	const fields = value as Record<string, unknown>;
	const keys = [
		'version',
		'tile_shape',
		'media_fit',
		'streaming_mode',
		'stream_limit',
		'keep_awake',
		'gap_px',
		'corner_radius_px'
	];
	const {
		version,
		tile_shape: tileShape,
		media_fit: mediaFit,
		streaming_mode: streamingMode,
		stream_limit: streamLimit,
		keep_awake: keepAwake,
		gap_px: gapPx,
		corner_radius_px: cornerRadiusPx
	} = fields;
	if (
		Object.keys(fields).some((key) => !keys.includes(key)) ||
		version !== 1 ||
		(tileShape !== '16:9' && tileShape !== '4:3' && tileShape !== 'native') ||
		(mediaFit !== 'contain' && mediaFit !== 'cover') ||
		(streamingMode !== 'smart' && streamingMode !== 'continuous') ||
		!isBoundedInteger(streamLimit, 1, maximumWallStreams) ||
		typeof keepAwake !== 'boolean' ||
		!isBoundedInteger(gapPx, 0, maximumWallDecorationPx) ||
		!isBoundedInteger(cornerRadiusPx, 0, maximumWallDecorationPx)
	) {
		throw new Error('Dashboard display settings are invalid.');
	}
	return {
		version,
		tileShape,
		mediaFit,
		streamingMode,
		streamLimit,
		keepAwake,
		gapPx,
		cornerRadiusPx
	};
}

function isBoundedInteger(value: unknown, minimum: number, maximum: number): value is number {
	return (
		typeof value === 'number' && Number.isInteger(value) && value >= minimum && value <= maximum
	);
}

export function stableNativeRatio(
	current: number | null,
	width: number,
	height: number
): number | null {
	if (current !== null) return current;
	if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) return null;
	const ratio = width / height;
	return ratio >= 1 / 16 && ratio <= 16 ? ratio : null;
}
