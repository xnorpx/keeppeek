import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { createReadStream } from 'node:fs';
import { access, mkdir, readFile, rename, rm, stat, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
	nineCameraKeyframeIntervalsSeconds,
	nineCameraProfileGopFrames,
	nineCameraProfileVariants,
	type NineCameraKeyframeIntervalSeconds,
	type NineCameraProfile
} from '../src/lib/server/storybook/nine-camera-fixture';

const sourceUrl =
	'https://download.blender.org/demo/movies/BBB/bbb_sunflower_2160p_30fps_normal.mp4.zip';
const sourceArchiveSha256 = '750b255c6d9fee1e2a03a6716d4f358bca56e9115bf3e06a66162fc5272ae151';
const sourceMediaSha256 = '37f0ff251a606c2dcfa26c19fe6bf843234b4e7a8889cfab50bc26f644e55520';
const sourceExcerptSha256 = '21be06202908ddfb5adaa53cb63f8b0564fcab446045bc37be7b8faece6a564c';
const sourceExcerptMaxBytes = 50_000_000;
const derivativeVersion = 4;
const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const fixtureDirectory = join(repositoryRoot, 'target', 'demo-fixtures');
const sourcePath = join(
	repositoryRoot,
	'crates',
	'test-camera',
	'testdata',
	'big-buck-bunny-3840x2160-h264.mp4'
);
const legacyManifestPath = join(fixtureDirectory, 'big-buck-bunny-640x360-h264.json');
const manifestPath = join(fixtureDirectory, 'big-buck-bunny-camera-profiles.json');
const force = process.argv.includes('--force');

type Probe = {
	streams: Array<{
		codec_name?: string;
		codec_type?: string;
		profile?: string;
		pix_fmt?: string;
		width?: number;
		height?: number;
		r_frame_rate?: string;
		bit_rate?: string;
		has_b_frames?: number;
		nb_frames?: string;
	}>;
	format: { duration?: string; size?: string };
};

type KeyframeProbe = {
	frames: Array<{ best_effort_timestamp_time?: string }>;
};

type FixtureProfile = NineCameraProfile & {
	outputFile: string;
	outputSha256: string;
	bytes: number;
};

type FixtureVariant = {
	keyframeIntervalSeconds: NineCameraKeyframeIntervalSeconds;
	profiles: FixtureProfile[];
};

type FixtureManifest = {
	schemaVersion: 1;
	derivativeVersion: number;
	title: string;
	sourceUrl: string;
	sourceArchiveSha256: string;
	sourceMediaSha256: string;
	license: string;
	attribution: string;
	sourceExcerptFile: string;
	sourceExcerptSha256: string;
	durationSeconds: number;
	variants: FixtureVariant[];
	blackIntervals: BlackInterval[];
};

type BlackInterval = {
	startSeconds: number;
	endSeconds: number;
};

await mkdir(fixtureDirectory, { recursive: true });
const sourceProbe = await validateSourceFixture();
if (!force && (await cachedFixture())) process.exit(0);

const variants: FixtureVariant[] = [];
const durationSeconds = Number(sourceProbe.format.duration);
for (const keyframeIntervalSeconds of nineCameraKeyframeIntervalsSeconds) {
	const profiles: FixtureProfile[] = [];
	for (const profile of nineCameraProfileVariants) {
		const outputFile = fixtureOutputPath(profile, keyframeIntervalSeconds);
		const temporaryOutput = `${outputFile}.tmp.mp4`;
		const gopFrames = nineCameraProfileGopFrames(profile, keyframeIntervalSeconds);
		await rm(temporaryOutput, { force: true });
		await runProcess('ffmpeg', [
			'-hide_banner',
			'-loglevel',
			'error',
			'-y',
			'-i',
			sourcePath,
			'-map',
			'0:v:0',
			'-vf',
			`scale=${profile.width}:${profile.height}:flags=lanczos,fps=${profile.framesPerSecond}`,
			'-an',
			...encoderArguments(profile),
			'-b:v',
			`${profile.bitrateKbps}k`,
			'-maxrate',
			`${profile.bitrateKbps}k`,
			'-bufsize',
			`${profile.bitrateKbps * 2}k`,
			'-pix_fmt',
			'yuv420p',
			'-g',
			String(gopFrames),
			'-keyint_min',
			String(gopFrames),
			'-sc_threshold',
			'0',
			'-bf',
			'0',
			'-map_metadata',
			'-1',
			'-movflags',
			'+faststart',
			temporaryOutput
		]);
		const probe = await validateFixture(temporaryOutput, profile, keyframeIntervalSeconds);
		if (Math.abs(durationSeconds - Number(probe.format.duration)) > 0.05) {
			throw new Error('Big Buck Bunny profile variants have different durations');
		}
		await rm(outputFile, { force: true });
		await rename(temporaryOutput, outputFile);
		const outputStats = await stat(outputFile);
		profiles.push({
			...profile,
			outputFile,
			outputSha256: await sha256(outputFile),
			bytes: outputStats.size
		});
	}
	variants.push({ keyframeIntervalSeconds, profiles });
}
const blackIntervals = await detectBlackIntervals(
	fixtureProfile(variants[0]!, 'sub').outputFile,
	durationSeconds
);
if (blackIntervals.length > 0) {
	throw new Error(
		`Committed Big Buck Bunny excerpt contains black frames: ${JSON.stringify(blackIntervals)}`
	);
}
await removeLegacyFixtures();

const manifest: FixtureManifest = {
	schemaVersion: 1,
	derivativeVersion,
	title: 'Big Buck Bunny',
	sourceUrl,
	sourceArchiveSha256,
	sourceMediaSha256,
	license: 'Creative Commons Attribution 3.0',
	attribution: '(c) copyright 2008, Blender Foundation / www.bigbuckbunny.org',
	sourceExcerptFile: sourcePath,
	sourceExcerptSha256,
	durationSeconds,
	variants,
	blackIntervals
};
await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
printFixture(manifest);

async function cachedFixture(): Promise<boolean> {
	try {
		const manifest = JSON.parse(await readFile(manifestPath, 'utf8')) as Omit<
			FixtureManifest,
			'blackIntervals'
		> & { blackIntervals?: BlackInterval[] };
		if (
			manifest.derivativeVersion !== derivativeVersion ||
			manifest.sourceArchiveSha256 !== sourceArchiveSha256 ||
			manifest.sourceMediaSha256 !== sourceMediaSha256 ||
			manifest.sourceExcerptFile !== sourcePath ||
			manifest.sourceExcerptSha256 !== sourceExcerptSha256 ||
			manifest.variants.length !== nineCameraKeyframeIntervalsSeconds.length
		) {
			return false;
		}
		for (const keyframeIntervalSeconds of nineCameraKeyframeIntervalsSeconds) {
			const variant = manifest.variants.find(
				(candidate) => candidate.keyframeIntervalSeconds === keyframeIntervalSeconds
			);
			if (!variant || variant.profiles.length !== nineCameraProfileVariants.length) return false;
			for (const profile of nineCameraProfileVariants) {
				const generated = variant.profiles.find(
					(candidate) => candidate.stream === profile.stream && candidate.codec === profile.codec
				);
				const outputFile = fixtureOutputPath(profile, keyframeIntervalSeconds);
				if (
					!generated ||
					generated.codec !== profile.codec ||
					generated.width !== profile.width ||
					generated.height !== profile.height ||
					generated.framesPerSecond !== profile.framesPerSecond ||
					generated.bitrateKbps !== profile.bitrateKbps ||
					generated.outputFile !== outputFile ||
					!(await matchesHash(outputFile, generated.outputSha256))
				) {
					return false;
				}
				const probe = await validateFixture(outputFile, profile, keyframeIntervalSeconds);
				if (Math.abs(Number(sourceProbe.format.duration) - Number(probe.format.duration)) > 0.05) {
					return false;
				}
			}
		}
		const durationSeconds = Number(sourceProbe.format.duration);
		const blackIntervals = validBlackIntervals(manifest.blackIntervals, durationSeconds)
			? manifest.blackIntervals
			: await detectBlackIntervals(
					fixtureProfile(manifest.variants[0]!, 'sub').outputFile,
					durationSeconds
				);
		if (blackIntervals.length > 0) return false;
		const completeManifest: FixtureManifest = {
			...manifest,
			durationSeconds,
			blackIntervals
		};
		if (blackIntervals !== manifest.blackIntervals) {
			await writeFile(manifestPath, `${JSON.stringify(completeManifest, null, 2)}\n`);
		}
		printFixture(completeManifest);
		return true;
	} catch {
		return false;
	}
}

async function detectBlackIntervals(
	filePath: string,
	durationSeconds: number
): Promise<BlackInterval[]> {
	const output = await runProcess(
		'ffmpeg',
		[
			'-hide_banner',
			'-loglevel',
			'error',
			'-i',
			filePath,
			'-vf',
			'blackdetect=d=0.1:pix_th=0.10,metadata=mode=print:file=-',
			'-an',
			'-f',
			'null',
			'-'
		],
		true
	);
	const intervals: BlackInterval[] = [];
	let startSeconds: number | undefined;
	for (const line of output.split('\n')) {
		const match = /^lavfi\.black_(start|end)=([0-9]+(?:\.[0-9]+)?)$/.exec(line.trim());
		if (!match) continue;
		const seconds = Number(match[2]);
		if (match[1] === 'start') {
			startSeconds = seconds;
		} else if (startSeconds !== undefined) {
			intervals.push({ startSeconds, endSeconds: seconds });
			startSeconds = undefined;
		}
	}
	if (startSeconds !== undefined && startSeconds < durationSeconds) {
		intervals.push({ startSeconds, endSeconds: durationSeconds });
	}
	if (!validBlackIntervals(intervals, durationSeconds)) {
		throw new Error(`Invalid black intervals: ${JSON.stringify(intervals)}`);
	}
	return intervals;
}

function validBlackIntervals(
	intervals: BlackInterval[] | undefined,
	durationSeconds: number
): intervals is BlackInterval[] {
	return (
		Array.isArray(intervals) &&
		intervals.every(
			(interval, index) =>
				Number.isFinite(interval.startSeconds) &&
				Number.isFinite(interval.endSeconds) &&
				interval.startSeconds >= 0 &&
				interval.endSeconds > interval.startSeconds &&
				interval.endSeconds <= durationSeconds &&
				(index === 0 || intervals[index - 1].endSeconds <= interval.startSeconds)
		)
	);
}

async function validateFixture(
	filePath: string,
	profile: NineCameraProfile,
	keyframeIntervalSeconds: NineCameraKeyframeIntervalSeconds
): Promise<Probe> {
	const probe = await probeFixture(filePath);
	const video = probe.streams[0];
	const expectedCodec = profile.codec === 'h264' ? 'h264' : 'hevc';
	const expectedProfile = profile.codec === 'h265' ? 'Main' : 'Constrained Baseline';
	const bitrate = Number(video?.bit_rate);
	const expectedFrames = 48 * profile.framesPerSecond;
	if (
		probe.streams.length !== 1 ||
		video?.codec_name !== expectedCodec ||
		video.codec_type !== 'video' ||
		video.profile !== expectedProfile ||
		video.pix_fmt !== 'yuv420p' ||
		video.width !== profile.width ||
		video.height !== profile.height ||
		video.r_frame_rate !== `${profile.framesPerSecond}/1` ||
		video.has_b_frames !== 0 ||
		Number(video.nb_frames) !== expectedFrames ||
		!Number.isFinite(bitrate) ||
		bitrate < profile.bitrateKbps * 750 ||
		bitrate > profile.bitrateKbps * 1_250 ||
		Number(probe.format.duration) < 47.9 ||
		Number(probe.format.duration) > 48.1
	) {
		throw new Error(`Unexpected Big Buck Bunny derivative: ${JSON.stringify(probe)}`);
	}
	await validateKeyframeInterval(filePath, keyframeIntervalSeconds, profile.framesPerSecond);
	return probe;
}

async function validateSourceFixture(): Promise<Probe> {
	if (!(await matchesHash(sourcePath, sourceExcerptSha256))) {
		throw new Error(
			`Committed 4K Big Buck Bunny source is missing or has the wrong SHA-256: ${sourcePath}`
		);
	}
	const sourceStats = await stat(sourcePath);
	const probe = await probeFixture(sourcePath);
	const video = probe.streams[0];
	const bitrate = Number(video?.bit_rate);
	if (
		sourceStats.size > sourceExcerptMaxBytes ||
		probe.streams.length !== 1 ||
		video?.codec_name !== 'h264' ||
		video.profile !== 'High' ||
		video.pix_fmt !== 'yuv420p' ||
		video.width !== 3840 ||
		video.height !== 2160 ||
		video.r_frame_rate !== '30/1' ||
		video.has_b_frames !== 0 ||
		Number(video.nb_frames) !== 1_440 ||
		!Number.isFinite(bitrate) ||
		bitrate < 6_000_000 ||
		bitrate > 9_000_000 ||
		Number(probe.format.duration) !== 48
	) {
		throw new Error(`Unexpected committed 4K Big Buck Bunny source: ${JSON.stringify(probe)}`);
	}
	await validateKeyframeInterval(sourcePath, 1, 30);
	return probe;
}

async function probeFixture(filePath: string): Promise<Probe> {
	const output = await runProcess(
		'ffprobe',
		[
			'-v',
			'error',
			'-count_frames',
			'-show_entries',
			'format=duration,size:stream=codec_name,codec_type,profile,pix_fmt,width,height,r_frame_rate,bit_rate,has_b_frames,nb_frames',
			'-of',
			'json',
			filePath
		],
		true
	);
	return JSON.parse(output) as Probe;
}

async function validateKeyframeInterval(
	filePath: string,
	expectedIntervalSeconds: NineCameraKeyframeIntervalSeconds,
	framesPerSecond: number
): Promise<void> {
	const output = await runProcess(
		'ffprobe',
		[
			'-v',
			'error',
			'-skip_frame',
			'nokey',
			'-select_streams',
			'v:0',
			'-show_entries',
			'frame=best_effort_timestamp_time',
			'-of',
			'json',
			filePath
		],
		true
	);
	const timestamps = (JSON.parse(output) as KeyframeProbe).frames.map((frame) =>
		Number(frame.best_effort_timestamp_time)
	);
	if (timestamps.length < 2 || timestamps.some((timestamp) => !Number.isFinite(timestamp))) {
		throw new Error(`Big Buck Bunny derivative has invalid keyframe timestamps: ${filePath}`);
	}
	const toleranceSeconds = 0.5 / framesPerSecond;
	for (let index = 1; index < timestamps.length; index += 1) {
		const intervalSeconds = timestamps[index]! - timestamps[index - 1]!;
		if (Math.abs(intervalSeconds - expectedIntervalSeconds) > toleranceSeconds) {
			throw new Error(
				`Big Buck Bunny derivative keyframe interval ${intervalSeconds}s does not match ${expectedIntervalSeconds}s`
			);
		}
	}
}

function encoderArguments(profile: NineCameraProfile): string[] {
	if (profile.codec === 'h265') {
		return [
			'-c:v',
			'libx265',
			'-preset',
			'ultrafast',
			'-profile:v',
			'main',
			'-x265-params',
			'log-level=error:open-gop=0',
			'-tag:v',
			'hvc1'
		];
	}
	return profile.stream === 'main'
		? ['-c:v', 'libx264', '-preset', 'veryfast', '-profile:v', 'baseline', '-level:v', '5.1']
		: ['-c:v', 'libx264', '-preset', 'veryfast', '-profile:v', 'baseline', '-level:v', '3.1'];
}

function fixtureOutputPath(
	profile: NineCameraProfile,
	keyframeIntervalSeconds: NineCameraKeyframeIntervalSeconds
): string {
	return join(
		fixtureDirectory,
		`big-buck-bunny-${profile.stream}-${profile.width}x${profile.height}-${profile.codec}-gop-${keyframeIntervalSeconds}s.mp4`
	);
}

function fixtureProfile(
	variant: FixtureVariant,
	stream: 'main' | 'sub',
	codec: 'h264' | 'h265' = 'h264'
): FixtureProfile {
	const profile = variant.profiles.find(
		(candidate) => candidate.stream === stream && candidate.codec === codec
	);
	if (!profile) {
		throw new Error(
			`Big Buck Bunny ${variant.keyframeIntervalSeconds}s fixture has no ${stream} ${codec} profile`
		);
	}
	return profile;
}

async function removeLegacyFixtures(): Promise<void> {
	await Promise.all([
		rm(legacyManifestPath, { force: true }),
		...nineCameraKeyframeIntervalsSeconds.map((interval) =>
			rm(join(fixtureDirectory, `big-buck-bunny-640x360-h264-gop-${interval}s.mp4`), {
				force: true
			})
		)
	]);
}

async function matchesHash(filePath: string, expected: string): Promise<boolean> {
	try {
		await access(filePath);
		return (await sha256(filePath)) === expected;
	} catch {
		return false;
	}
}

async function sha256(filePath: string): Promise<string> {
	const hash = createHash('sha256');
	for await (const chunk of createReadStream(filePath)) hash.update(chunk);
	return hash.digest('hex');
}

async function runProcess(command: string, args: string[], captureStdout = false): Promise<string> {
	return new Promise((resolveProcess, rejectProcess) => {
		const processHandle = spawn(command, args, {
			stdio: captureStdout ? ['ignore', 'pipe', 'pipe'] : 'inherit'
		});
		let stdout = '';
		let stderr = '';
		if (captureStdout) {
			processHandle.stdout?.setEncoding('utf8');
			processHandle.stderr?.setEncoding('utf8');
			processHandle.stdout?.on('data', (chunk: string) => (stdout += chunk));
			processHandle.stderr?.on('data', (chunk: string) => (stderr += chunk));
		}
		processHandle.on('error', rejectProcess);
		processHandle.on('close', (exitCode) => {
			if (exitCode !== 0) {
				rejectProcess(new Error(`${command} failed: ${stderr.trim()}`));
				return;
			}
			resolveProcess(stdout);
		});
	});
}

function printFixture(manifest: FixtureManifest): void {
	console.log(
		JSON.stringify({
			sourceExcerpt: {
				file: manifest.sourceExcerptFile,
				sha256: manifest.sourceExcerptSha256,
				bytes: Number(sourceProbe.format.size)
			},
			durationSeconds: manifest.durationSeconds,
			variants: manifest.variants
		})
	);
}
