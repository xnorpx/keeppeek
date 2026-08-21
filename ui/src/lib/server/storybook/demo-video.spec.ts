import { describe, expect, it } from 'vitest';
import {
	assertDemoRecordingCovers,
	assertH264AacVideo,
	assertH264OnlyVideo,
	createFfprobeDurationArgs,
	createFfprobeStreamsArgs,
	createNarratedDemoPlan,
	createPacedDemoVideoMuxArgs,
	createSilentDemoVideoMuxArgs,
	parseFfprobeDurationMs
} from './demo-video';

describe('demo video muxing', () => {
	it('lets narration delay the next visual phase by freezing the final frame', () => {
		const cues = [
			{ sourceAtMs: 0, audioPath: 'first.wav', audioDurationMs: 2_600, pauseAfterMs: 400 },
			{ sourceAtMs: 2_000, audioPath: 'then.wav', audioDurationMs: 1_500 }
		] as const;
		expect(createNarratedDemoPlan(5_000, cues)).toEqual({
			outputDurationMs: 6_000,
			segments: [
				{
					sourceStartMs: 0,
					sourceEndMs: 2_000,
					outputStartMs: 0,
					outputDurationMs: 3_000,
					audioDurationMs: 2_600,
					freezeDurationMs: 1_000
				},
				{
					sourceStartMs: 2_000,
					sourceEndMs: 5_000,
					outputStartMs: 3_000,
					outputDurationMs: 3_000,
					audioDurationMs: 1_500,
					freezeDurationMs: 0
				}
			]
		});

		const args = createPacedDemoVideoMuxArgs({
			videoPath: 'silent.mp4',
			outputPath: 'narrated.mp4',
			sourceDurationMs: 5_000,
			cues
		});
		expect(args).toEqual(
			expect.arrayContaining([
				'first.wav',
				'then.wav',
				'[0:v]trim=start=0.000:end=2.000,setpts=PTS-STARTPTS,tpad=stop_mode=clone:stop_duration=1.000[v0];[1:a]aresample=48000,apad,atrim=duration=3.000,asetpts=PTS-STARTPTS[a0];[0:v]trim=start=2.000:end=5.000,setpts=PTS-STARTPTS[v1];[2:a]aresample=48000,apad,atrim=duration=3.000,asetpts=PTS-STARTPTS[a1];[v0][a0][v1][a1]concat=n=2:v=1:a=1[video][narration]',
				'narrated.mp4'
			])
		);
	});

	it('rejects narration cues that do not partition the source timeline', () => {
		expect(() =>
			createNarratedDemoPlan(5_000, [
				{ sourceAtMs: 500, audioPath: 'late.wav', audioDurationMs: 1_000 }
			])
		).toThrow('source time zero');
		expect(() =>
			createNarratedDemoPlan(5_000, [
				{ sourceAtMs: 0, audioPath: 'first.wav', audioDurationMs: 1_000 },
				{ sourceAtMs: 0, audioPath: 'duplicate.wav', audioDurationMs: 1_000 }
			])
		).toThrow('must increase');
	});

	it('builds and parses an ffprobe duration request', () => {
		expect(createFfprobeDurationArgs('narration.wav')).toEqual([
			'-v',
			'error',
			'-show_entries',
			'format=duration',
			'-of',
			'default=noprint_wrappers=1:nokey=1',
			'narration.wav'
		]);
		expect(parseFfprobeDurationMs('6.125000\n')).toBe(6_125);
		expect(() => parseFfprobeDurationMs('N/A')).toThrow('invalid media duration');
	});

	it('requires exactly one H.264 yuv420p video stream', () => {
		expect(createFfprobeStreamsArgs('demo.mp4')).toEqual([
			'-v',
			'error',
			'-show_entries',
			'stream=codec_name,codec_type,pix_fmt,duration',
			'-of',
			'json',
			'demo.mp4'
		]);
		expect(() =>
			assertH264OnlyVideo(
				JSON.stringify({
					streams: [{ codec_name: 'h264', codec_type: 'video', pix_fmt: 'yuv420p' }]
				})
			)
		).not.toThrow();
		expect(() =>
			assertH264OnlyVideo(
				JSON.stringify({
					streams: [
						{ codec_name: 'h264', codec_type: 'video', pix_fmt: 'yuv420p' },
						{ codec_name: 'mov_text', codec_type: 'subtitle' }
					]
				})
			)
		).toThrow('Expected one H.264');
	});

	it('requires one H.264 video and one AAC narration stream', () => {
		expect(() =>
			assertH264AacVideo(
				JSON.stringify({
					streams: [
						{
							codec_name: 'h264',
							codec_type: 'video',
							pix_fmt: 'yuv420p',
							duration: '6.000'
						},
						{ codec_name: 'aac', codec_type: 'audio', duration: '6.000' }
					]
				}),
				6_000
			)
		).not.toThrow();
		expect(() =>
			assertH264AacVideo(
				JSON.stringify({
					streams: [{ codec_name: 'h264', codec_type: 'video', pix_fmt: 'yuv420p' }]
				})
			)
		).toThrow('with AAC audio');
		expect(() =>
			assertH264AacVideo(
				JSON.stringify({
					streams: [
						{
							codec_name: 'h264',
							codec_type: 'video',
							pix_fmt: 'yuv420p',
							duration: '6.000'
						},
						{ codec_name: 'aac', codec_type: 'audio', duration: '5.500' }
					]
				}),
				6_000
			)
		).toThrow('stream duration');
	});

	it('builds a silent captioned MP4 from one trimmed Playwright recording', () => {
		expect(
			createSilentDemoVideoMuxArgs({
				videoPath: 'capture.webm',
				captionsPath: 'captions.vtt',
				outputPath: 'demo.mp4',
				durationMs: 9_000,
				recordingPreRollMs: 420
			})
		).toEqual(
			expect.arrayContaining([
				'capture.webm',
				'captions.vtt',
				'[0:v]trim=start=0.420:duration=9.000,setpts=PTS-STARTPTS[video]',
				'libx264',
				'-an',
				'mov_text',
				'demo.mp4'
			])
		);
	});

	it('rejects a recording that does not cover the authored source timeline', () => {
		expect(() =>
			assertDemoRecordingCovers({
				demoDurationMs: 9_000,
				videoDurationMs: 9_200,
				recordingPreRollMs: 420
			})
		).toThrow('recording does not cover');
	});
});
