export type DemoVideoMuxOptions = {
	videoPath: string;
	audioPath: string;
	captionsPath?: string;
	outputPath: string;
	durationMs: number;
	recordingPreRollMs: number;
	audioDelayMs?: number;
};

export type SilentDemoVideoMuxOptions = {
	videoPath: string;
	captionsPath?: string;
	outputPath: string;
	durationMs: number;
	recordingPreRollMs: number;
};

function requireNonNegativeInteger(name: string, value: number): void {
	if (!Number.isInteger(value) || value < 0) {
		throw new Error(`${name} must be a non-negative integer`);
	}
}

function formatSeconds(timeMs: number): string {
	return (timeMs / 1_000).toFixed(3);
}

export function createFfprobeDurationArgs(mediaPath: string): string[] {
	return [
		'-v',
		'error',
		'-show_entries',
		'format=duration',
		'-of',
		'default=noprint_wrappers=1:nokey=1',
		mediaPath
	];
}

export function parseFfprobeDurationMs(output: string): number {
	const durationSeconds = Number(output.trim());
	if (!Number.isFinite(durationSeconds) || durationSeconds <= 0) {
		throw new Error('ffprobe returned an invalid media duration');
	}
	return Math.round(durationSeconds * 1_000);
}

export function createFfprobeStreamsArgs(mediaPath: string): string[] {
	return [
		'-v',
		'error',
		'-show_entries',
		'stream=codec_name,codec_type,pix_fmt',
		'-of',
		'json',
		mediaPath
	];
}

export function assertH264OnlyVideo(output: string): void {
	let probe: {
		streams?: Array<{ codec_name?: string; codec_type?: string; pix_fmt?: string }>;
	};
	try {
		probe = JSON.parse(output) as typeof probe;
	} catch {
		throw new Error('ffprobe returned invalid stream metadata');
	}
	if (
		probe.streams?.length !== 1 ||
		probe.streams[0].codec_name !== 'h264' ||
		probe.streams[0].codec_type !== 'video' ||
		probe.streams[0].pix_fmt !== 'yuv420p'
	) {
		throw new Error(`Expected one H.264 yuv420p video stream, received ${output.trim()}`);
	}
}

export function assertDemoMediaFits(options: {
	demoDurationMs: number;
	videoDurationMs: number;
	recordingPreRollMs: number;
	narrationDurationMs: number;
	audioDelayMs: number;
}): void {
	requireNonNegativeInteger('demoDurationMs', options.demoDurationMs);
	requireNonNegativeInteger('videoDurationMs', options.videoDurationMs);
	requireNonNegativeInteger('recordingPreRollMs', options.recordingPreRollMs);
	requireNonNegativeInteger('narrationDurationMs', options.narrationDurationMs);
	requireNonNegativeInteger('audioDelayMs', options.audioDelayMs);

	assertDemoRecordingCovers(options);
	if (options.audioDelayMs + options.narrationDurationMs > options.demoDurationMs) {
		throw new Error('Azure OpenAI narration exceeds the demo timeline');
	}
}

export function assertDemoRecordingCovers(options: {
	demoDurationMs: number;
	videoDurationMs: number;
	recordingPreRollMs: number;
}): void {
	requireNonNegativeInteger('demoDurationMs', options.demoDurationMs);
	requireNonNegativeInteger('videoDurationMs', options.videoDurationMs);
	requireNonNegativeInteger('recordingPreRollMs', options.recordingPreRollMs);
	if (options.demoDurationMs === 0) throw new Error('demoDurationMs must be greater than zero');
	if (options.recordingPreRollMs + options.demoDurationMs > options.videoDurationMs) {
		throw new Error('Playwright recording does not cover the complete demo timeline');
	}
}

export function createSilentDemoVideoMuxArgs(options: SilentDemoVideoMuxOptions): string[] {
	requireNonNegativeInteger('recordingPreRollMs', options.recordingPreRollMs);
	if (!Number.isInteger(options.durationMs) || options.durationMs <= 0) {
		throw new Error('durationMs must be a positive integer');
	}
	const durationSeconds = formatSeconds(options.durationMs);
	return [
		'-y',
		'-i',
		options.videoPath,
		...(options.captionsPath === undefined ? [] : ['-i', options.captionsPath]),
		'-filter_complex',
		`[0:v]trim=start=${formatSeconds(options.recordingPreRollMs)}:duration=${durationSeconds},setpts=PTS-STARTPTS[video]`,
		'-map',
		'[video]',
		...(options.captionsPath === undefined ? [] : ['-map', '1:s:0']),
		'-c:v',
		'libx264',
		'-preset',
		'medium',
		'-crf',
		'18',
		'-pix_fmt',
		'yuv420p',
		'-an',
		...(options.captionsPath === undefined ? [] : ['-c:s', 'mov_text']),
		'-movflags',
		'+faststart',
		options.outputPath
	];
}

export function createDemoVideoMuxArgs(options: DemoVideoMuxOptions): string[] {
	const audioDelayMs = options.audioDelayMs ?? 0;
	requireNonNegativeInteger('audioDelayMs', audioDelayMs);
	requireNonNegativeInteger('recordingPreRollMs', options.recordingPreRollMs);
	if (!Number.isInteger(options.durationMs) || options.durationMs <= 0) {
		throw new Error('durationMs must be a positive integer');
	}

	const durationSeconds = formatSeconds(options.durationMs);
	const filter = [
		`[0:v]trim=start=${formatSeconds(options.recordingPreRollMs)}:duration=${durationSeconds},setpts=PTS-STARTPTS[video]`,
		`[1:a]adelay=${audioDelayMs}:all=1,apad,atrim=duration=${durationSeconds}[narration]`
	].join(';');

	const args = [
		'-y',
		'-i',
		options.videoPath,
		'-i',
		options.audioPath,
		...(options.captionsPath === undefined ? [] : ['-i', options.captionsPath]),
		'-filter_complex',
		filter,
		'-map',
		'[video]',
		'-map',
		'[narration]',
		...(options.captionsPath === undefined ? [] : ['-map', '2:s:0']),
		'-c:v',
		'libx264',
		'-preset',
		'medium',
		'-crf',
		'18',
		'-pix_fmt',
		'yuv420p',
		'-c:a',
		'aac',
		'-b:a',
		'96k',
		...(options.captionsPath === undefined ? [] : ['-c:s', 'mov_text']),
		'-movflags',
		'+faststart',
		options.outputPath
	];

	return args;
}
