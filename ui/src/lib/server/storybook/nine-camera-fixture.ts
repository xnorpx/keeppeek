export const nineCameraKeyframeIntervalsSeconds = [1, 2] as const;

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

export function nineCameraProfileGopFrames(
	profile: NineCameraProfile,
	keyframeIntervalSeconds: NineCameraKeyframeIntervalSeconds
): number {
	return profile.framesPerSecond * keyframeIntervalSeconds;
}
