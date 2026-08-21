export type SilentDemoVideoMuxOptions = {
	videoPath: string;
	captionsPath?: string;
	outputPath: string;
	durationMs: number;
	recordingPreRollMs: number;
};

export type NarrationCueMedia = {
	sourceAtMs: number;
	audioPath: string;
	audioDurationMs: number;
	pauseAfterMs?: number;
};

export type NarratedDemoSegment = {
	sourceStartMs: number;
	sourceEndMs: number;
	outputStartMs: number;
	outputDurationMs: number;
	audioDurationMs: number;
	freezeDurationMs: number;
};

export type NarratedDemoPlan = {
	outputDurationMs: number;
	segments: NarratedDemoSegment[];
};

export type PacedDemoVideoMuxOptions = {
	videoPath: string;
	outputPath: string;
	sourceDurationMs: number;
	cues: readonly NarrationCueMedia[];
};

function requireNonNegativeInteger(name: string, value: number): void {
	if (!Number.isInteger(value) || value < 0) {
		throw new Error(`${name} must be a non-negative integer`);
	}
}

function formatSeconds(timeMs: number): string {
	return (timeMs / 1_000).toFixed(3);
}

export function createNarratedDemoPlan(
	sourceDurationMs: number,
	cues: readonly NarrationCueMedia[]
): NarratedDemoPlan {
	if (!Number.isInteger(sourceDurationMs) || sourceDurationMs <= 0) {
		throw new Error('sourceDurationMs must be a positive integer');
	}
	if (cues.length === 0) throw new Error('Narrated demos require at least one cue');

	let outputStartMs = 0;
	const segments = cues.map((cue, index): NarratedDemoSegment => {
		requireNonNegativeInteger(`cues[${index}].sourceAtMs`, cue.sourceAtMs);
		if (!Number.isInteger(cue.audioDurationMs) || cue.audioDurationMs <= 0) {
			throw new Error(`cues[${index}].audioDurationMs must be a positive integer`);
		}
		const pauseAfterMs = cue.pauseAfterMs ?? 0;
		requireNonNegativeInteger(`cues[${index}].pauseAfterMs`, pauseAfterMs);
		if (index === 0 && cue.sourceAtMs !== 0) {
			throw new Error('The first narration cue must start at source time zero');
		}
		const nextSourceAtMs = cues[index + 1]?.sourceAtMs ?? sourceDurationMs;
		if (nextSourceAtMs <= cue.sourceAtMs || nextSourceAtMs > sourceDurationMs) {
			throw new Error('Narration cue source times must increase within the source video');
		}

		const sourceSegmentDurationMs = nextSourceAtMs - cue.sourceAtMs;
		const outputDurationMs = Math.max(sourceSegmentDurationMs, cue.audioDurationMs + pauseAfterMs);
		const segment = {
			sourceStartMs: cue.sourceAtMs,
			sourceEndMs: nextSourceAtMs,
			outputStartMs,
			outputDurationMs,
			audioDurationMs: cue.audioDurationMs,
			freezeDurationMs: outputDurationMs - sourceSegmentDurationMs
		};
		outputStartMs += outputDurationMs;
		return segment;
	});

	return { outputDurationMs: outputStartMs, segments };
}

export function createPacedDemoVideoMuxArgs(options: PacedDemoVideoMuxOptions): string[] {
	const plan = createNarratedDemoPlan(options.sourceDurationMs, options.cues);
	const filters = plan.segments.flatMap((segment, index) => {
		const freeze =
			segment.freezeDurationMs === 0
				? ''
				: `,tpad=stop_mode=clone:stop_duration=${formatSeconds(segment.freezeDurationMs)}`;
		return [
			`[0:v]trim=start=${formatSeconds(segment.sourceStartMs)}:end=${formatSeconds(segment.sourceEndMs)},setpts=PTS-STARTPTS${freeze}[v${index}]`,
			`[${index + 1}:a]aresample=48000,apad,atrim=duration=${formatSeconds(segment.outputDurationMs)},asetpts=PTS-STARTPTS[a${index}]`
		];
	});
	const concatInputs = plan.segments.map((_, index) => `[v${index}][a${index}]`).join('');
	filters.push(`${concatInputs}concat=n=${plan.segments.length}:v=1:a=1[video][narration]`);

	return [
		'-y',
		'-i',
		options.videoPath,
		...options.cues.flatMap((cue) => ['-i', cue.audioPath]),
		'-filter_complex',
		filters.join(';'),
		'-map',
		'[video]',
		'-map',
		'[narration]',
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
		'-movflags',
		'+faststart',
		options.outputPath
	];
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
		'stream=codec_name,codec_type,pix_fmt,duration',
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

export function assertH264AacVideo(output: string, expectedDurationMs?: number): void {
	let probe: {
		streams?: Array<{
			codec_name?: string;
			codec_type?: string;
			pix_fmt?: string;
			duration?: string;
		}>;
	};
	try {
		probe = JSON.parse(output) as typeof probe;
	} catch {
		throw new Error('ffprobe returned invalid stream metadata');
	}
	const videoStreams = probe.streams?.filter((stream) => stream.codec_type === 'video') ?? [];
	const audioStreams = probe.streams?.filter((stream) => stream.codec_type === 'audio') ?? [];
	if (
		probe.streams?.length !== 2 ||
		videoStreams.length !== 1 ||
		videoStreams[0].codec_name !== 'h264' ||
		videoStreams[0].pix_fmt !== 'yuv420p' ||
		audioStreams.length !== 1 ||
		audioStreams[0].codec_name !== 'aac'
	) {
		throw new Error(`Expected H.264 yuv420p video with AAC audio, received ${output.trim()}`);
	}
	if (expectedDurationMs !== undefined) {
		for (const stream of probe.streams) {
			const durationMs = Number(stream.duration) * 1_000;
			if (!Number.isFinite(durationMs) || Math.abs(durationMs - expectedDurationMs) > 100) {
				throw new Error(`Media stream duration does not match ${expectedDurationMs}ms`);
			}
		}
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
