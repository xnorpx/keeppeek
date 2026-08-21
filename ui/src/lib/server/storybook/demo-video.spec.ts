import { describe, expect, it } from 'vitest';
import {
	assertDemoMediaFits,
	assertDemoRecordingCovers,
	assertH264OnlyVideo,
	createDemoVideoMuxArgs,
	createFfprobeDurationArgs,
	createFfprobeStreamsArgs,
	createSilentDemoVideoMuxArgs,
	parseFfprobeDurationMs
} from './demo-video';

describe('demo video muxing', () => {
	it('builds an MP4 mux command with delayed narration and captions', () => {
		const args = createDemoVideoMuxArgs({
			videoPath: 'capture.webm',
			audioPath: 'narration.wav',
			captionsPath: 'captions.vtt',
			outputPath: 'demo.mp4',
			durationMs: 9_000,
			recordingPreRollMs: 420,
			audioDelayMs: 500
		});

		expect(args).toEqual(
			expect.arrayContaining([
				'capture.webm',
				'narration.wav',
				'captions.vtt',
				'[0:v]trim=start=0.420:duration=9.000,setpts=PTS-STARTPTS[video];[1:a]adelay=500:all=1,apad,atrim=duration=9.000[narration]',
				'libx264',
				'aac',
				'mov_text',
				'demo.mp4'
			])
		);
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
			'stream=codec_name,codec_type,pix_fmt',
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

	it('accepts media that fits one explicit demo timeline', () => {
		expect(() =>
			assertDemoMediaFits({
				demoDurationMs: 9_000,
				videoDurationMs: 9_500,
				recordingPreRollMs: 420,
				narrationDurationMs: 7_500,
				audioDelayMs: 500
			})
		).not.toThrow();
	});

	it('rejects incomplete video and narration that overruns the demo', () => {
		expect(() =>
			assertDemoRecordingCovers({
				demoDurationMs: 9_000,
				videoDurationMs: 9_200,
				recordingPreRollMs: 420
			})
		).toThrow('recording does not cover');
		expect(() =>
			assertDemoMediaFits({
				demoDurationMs: 9_000,
				videoDurationMs: 9_200,
				recordingPreRollMs: 420,
				narrationDurationMs: 7_500,
				audioDelayMs: 500
			})
		).toThrow('recording does not cover');
		expect(() =>
			assertDemoMediaFits({
				demoDurationMs: 9_000,
				videoDurationMs: 9_500,
				recordingPreRollMs: 420,
				narrationDurationMs: 8_600,
				audioDelayMs: 500
			})
		).toThrow('narration exceeds');
	});

	it('rejects a negative narration delay', () => {
		expect(() =>
			createDemoVideoMuxArgs({
				videoPath: 'capture.webm',
				audioPath: 'narration.wav',
				outputPath: 'demo.mp4',
				durationMs: 9_000,
				recordingPreRollMs: 0,
				audioDelayMs: -1
			})
		).toThrow('audioDelayMs must be a non-negative integer');
	});
});
