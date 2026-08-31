import { describe, expect, it } from 'vitest';
import {
	nineCameraCircularStartSeparationSeconds,
	nineCameraKeyframeIntervalsSeconds,
	nineCameraKeyframeIntervalSeconds,
	nineCameraMinimumStartSeparationSeconds,
	nineCameraProfileGopFrames,
	nineCameraProfiles,
	nineCameraProfileVariants
} from './nine-camera-fixture';

describe('nine-camera fixture', () => {
	it('alternates one- and two-second keyframe intervals', () => {
		expect(nineCameraKeyframeIntervalsSeconds).toEqual([1, 2]);
		expect(
			Array.from({ length: 9 }, (_, index) => nineCameraKeyframeIntervalSeconds(index))
		).toEqual([1, 2, 1, 2, 1, 2, 1, 2, 1]);
		expect(nineCameraProfileVariants).toEqual([
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
		]);
		expect(
			nineCameraProfileVariants.map((profile) =>
				nineCameraKeyframeIntervalsSeconds.map((interval) =>
					nineCameraProfileGopFrames(profile, interval)
				)
			)
		).toEqual([
			[25, 50],
			[25, 50],
			[15, 30],
			[15, 30]
		]);
		expect(Array.from({ length: 6 }, (_, index) => nineCameraProfiles(index))).toEqual([
			[nineCameraProfileVariants[0], nineCameraProfileVariants[2]],
			[nineCameraProfileVariants[1], nineCameraProfileVariants[2]],
			[nineCameraProfileVariants[0], nineCameraProfileVariants[3]],
			[nineCameraProfileVariants[0], nineCameraProfileVariants[2]],
			[nineCameraProfileVariants[1], nineCameraProfileVariants[2]],
			[nineCameraProfileVariants[0], nineCameraProfileVariants[3]]
		]);
		const coverage = new Set(
			Array.from({ length: 9 }, (_, index) =>
				nineCameraProfiles(index).map(
					(profile) =>
						`${profile.stream}:${profile.codec}:${nineCameraKeyframeIntervalSeconds(index)}s`
				)
			).flat()
		);
		expect([...coverage].toSorted()).toEqual([
			'main:h264:1s',
			'main:h264:2s',
			'main:h265:1s',
			'main:h265:2s',
			'sub:h264:1s',
			'sub:h264:2s',
			'sub:h265:1s',
			'sub:h265:2s'
		]);
	});

	it('measures minimum start separation across the source loop boundary', () => {
		expect(nineCameraMinimumStartSeparationSeconds).toBe(1);
		expect(nineCameraCircularStartSeparationSeconds([0.6, 3.4, 47.5], 48)).toBeCloseTo(1.1);
		expect(nineCameraCircularStartSeparationSeconds([1, 1, 30], 48)).toBe(0);
	});
});
