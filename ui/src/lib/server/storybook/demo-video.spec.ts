import { execFileSync } from 'node:child_process';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
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
	finalizeDemoRecordingDirectory,
	parseFfprobeDurationMs
} from './demo-video';

describe('demo video muxing', () => {
	it.each([25, 30])(
		'keeps %i fps source narration on the planned timeline across fractional-frame cues',
		async (frameRate) => {
			const root = await mkdtemp(join(tmpdir(), 'keeppeek-demo-timing-'));
			const run = (command: string, args: string[]) =>
				execFileSync(command, args, {
					encoding: 'utf8',
					timeout: 30_000,
					stdio: ['ignore', 'pipe', 'pipe']
				});
			try {
				const videoPath = join(root, 'source.mp4');
				const outputPath = join(root, 'narrated.mp4');
				run('ffmpeg', [
					'-y',
					'-f',
					'lavfi',
					'-i',
					`color=size=64x64:rate=${frameRate}:duration=21`,
					'-c:v',
					'libx264',
					videoPath
				]);
				const cues = [
					[0, 4_850, 250],
					[1_800, 7_000, 250],
					[4_500, 7_650, 250],
					[6_500, 5_550, 250],
					[12_000, 9_600, 300],
					[16_000, 6_400, 300]
				].map(([sourceAtMs, audioDurationMs, pauseAfterMs], index) => {
					const audioPath = join(root, `cue-${index}.wav`);
					run('ffmpeg', [
						'-y',
						'-f',
						'lavfi',
						'-i',
						`sine=sample_rate=24000:duration=${audioDurationMs / 1_000}`,
						audioPath
					]);
					return { sourceAtMs, audioDurationMs, pauseAfterMs, audioPath };
				});
				const plan = createNarratedDemoPlan(21_000, cues);
				run(
					'ffmpeg',
					createPacedDemoVideoMuxArgs({ videoPath, outputPath, sourceDurationMs: 21_000, cues })
				);
				const durationMs = parseFfprobeDurationMs(
					run('ffprobe', createFfprobeDurationArgs(outputPath))
				);
				expect(Math.abs(durationMs - plan.outputDurationMs)).toBeLessThanOrEqual(40);
				assertH264AacVideo(
					run('ffprobe', createFfprobeStreamsArgs(outputPath)),
					plan.outputDurationMs
				);
			} finally {
				await rm(root, { recursive: true, force: true });
			}
		},
		60_000
	);

	it('rounds narration up and partitions source frames without losing boundary frames', () => {
		const cues = [
			{ sourceAtMs: 0, audioPath: 'first.wav', audioDurationMs: 41, pauseAfterMs: 40 },
			{ sourceAtMs: 50, audioPath: 'second.wav', audioDurationMs: 40 },
			{ sourceAtMs: 100, audioPath: 'last.wav', audioDurationMs: 1 }
		];
		const plan = createNarratedDemoPlan(201, cues);
		expect(plan.segments.map((segment) => segment.outputStartMs)).toEqual([0, 120, 160]);
		expect(plan.segments.map((segment) => segment.outputDurationMs)).toEqual([120, 40, 120]);
		expect(plan.segments.map((segment) => segment.freezeDurationMs)).toEqual([40, 0, 0]);
		expect(plan.outputDurationMs).toBe(280);
	});

	it('rejects source segments that cannot supply a frame to freeze', () => {
		expect(() =>
			createNarratedDemoPlan(100, [
				{ sourceAtMs: 0, audioPath: 'first.wav', audioDurationMs: 50 },
				{ sourceAtMs: 1, audioPath: 'second.wav', audioDurationMs: 50 },
				{ sourceAtMs: 2, audioPath: 'last.wav', audioDurationMs: 50 }
			])
		).toThrow('must contain a video frame');
	});

	it('retains failed recordings and removes successful raw captures', async () => {
		const root = await mkdtemp(join(tmpdir(), 'keeppeek-demo-recording-'));
		const recordingDirectory = join(root, 'recordings');
		const recordingPath = join(recordingDirectory, 'failure.webm');
		try {
			await mkdir(recordingDirectory);
			await writeFile(recordingPath, 'recording');
			await finalizeDemoRecordingDirectory({ recordingDirectory, completed: false });
			expect(await readFile(recordingPath, 'utf8')).toBe('recording');

			await finalizeDemoRecordingDirectory({ recordingDirectory, completed: true });
			await expect(readFile(recordingPath, 'utf8')).rejects.toThrow();
		} finally {
			await rm(root, { recursive: true, force: true });
		}
	});

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
				'[0:v]fps=25,trim=start_frame=0:end_frame=50,setpts=PTS-STARTPTS,tpad=stop_mode=clone:stop_duration=1.000[v0];[1:a]aresample=48000,apad,atrim=duration=3.000,asetpts=PTS-STARTPTS[a0];[0:v]fps=25,trim=start_frame=50:end_frame=125,setpts=PTS-STARTPTS[v1];[2:a]aresample=48000,apad,atrim=duration=3.000,asetpts=PTS-STARTPTS[a1];[v0][a0][v1][a1]concat=n=2:v=1:a=1[video][narration]',
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
