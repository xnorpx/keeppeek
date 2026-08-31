export const nineCameraKeyframeIntervalsSeconds = [1, 2] as const;
export const nineCameraMinimumStartSeparationSeconds = 1;

export const nineCameraProfileVariants = [
	{
		stream: 'main',
		codec: 'h264',
		width: 3840,
		height: 2160,
		framesPerSecond: 25,
		bitrateKbps: 8192
	},
	{
		stream: 'main',
		codec: 'h265',
		width: 3840,
		height: 2160,
		framesPerSecond: 25,
		bitrateKbps: 8192
	},
	{
		stream: 'sub',
		codec: 'h264',
		width: 640,
		height: 360,
		framesPerSecond: 15,
		bitrateKbps: 512
	},
	{
		stream: 'sub',
		codec: 'h265',
		width: 640,
		height: 360,
		framesPerSecond: 15,
		bitrateKbps: 256
	}
] as const;

const nineCameraCodecPairs = [
	{ main: 'h264', sub: 'h264' },
	{ main: 'h265', sub: 'h264' },
	{ main: 'h264', sub: 'h265' }
] as const;

export type NineCameraKeyframeIntervalSeconds = (typeof nineCameraKeyframeIntervalsSeconds)[number];
export type NineCameraProfile = (typeof nineCameraProfileVariants)[number];

export function nineCameraKeyframeIntervalSeconds(
	cameraIndex: number
): NineCameraKeyframeIntervalSeconds {
	if (!Number.isInteger(cameraIndex) || cameraIndex < 0) {
		throw new Error('Nine-camera fixture index must be a non-negative integer');
	}
	return nineCameraKeyframeIntervalsSeconds[
		cameraIndex % nineCameraKeyframeIntervalsSeconds.length
	]!;
}

export function nineCameraProfiles(cameraIndex: number): readonly NineCameraProfile[] {
	if (!Number.isInteger(cameraIndex) || cameraIndex < 0) {
		throw new Error('Nine-camera fixture index must be a non-negative integer');
	}
	const pair = nineCameraCodecPairs[cameraIndex % nineCameraCodecPairs.length]!;
	return (['main', 'sub'] as const).map((stream) => {
		const codec = pair[stream];
		return nineCameraProfileVariants.find(
			(profile) => profile.stream === stream && profile.codec === codec
		)!;
	});
}

export function nineCameraCircularStartSeparationSeconds(
	starts: readonly number[],
	durationSeconds: number
): number {
	if (!Number.isFinite(durationSeconds) || durationSeconds <= 0) {
		throw new Error('Nine-camera fixture duration must be positive');
	}
	if (starts.length < 2) {
		throw new Error('Nine-camera fixture needs at least two start offsets');
	}
	if (starts.some((start) => !Number.isFinite(start) || start < 0 || start >= durationSeconds)) {
		throw new Error('Nine-camera fixture start offsets must be inside the source duration');
	}
	const ordered = starts.toSorted((left, right) => left - right);
	return Math.min(
		...ordered.map((start, index) => {
			const next = ordered[(index + 1) % ordered.length]!;
			return next + (index === ordered.length - 1 ? durationSeconds : 0) - start;
		})
	);
}

export function nineCameraProfileGopFrames(
	profile: NineCameraProfile,
	keyframeIntervalSeconds: NineCameraKeyframeIntervalSeconds
): number {
	return profile.framesPerSecond * keyframeIntervalSeconds;
}

export function withinRelativeTolerance(
	value: number | null,
	target: number,
	tolerance: number
): boolean {
	return (
		value !== null &&
		Number.isFinite(value) &&
		Number.isFinite(target) &&
		target > 0 &&
		Number.isFinite(tolerance) &&
		tolerance >= 0 &&
		value >= target * (1 - tolerance) &&
		value <= target * (1 + tolerance)
	);
}
