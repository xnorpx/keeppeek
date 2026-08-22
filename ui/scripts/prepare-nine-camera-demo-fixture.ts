import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { createReadStream } from 'node:fs';
import { access, mkdir, readFile, rename, rm, stat, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const sourceUrl =
	'https://download.blender.org/peach/bigbuckbunny_movies/BigBuckBunny_640x360.m4v.zip';
const sourceArchiveSha256 = '7118242b6728d40c871479c5b3c0f0fb27d748089df15d7f1b469f297c74a2d6';
const sourceMediaSha256 = '738e2f999860553d056dd79c952f58f63cbb73892a57c72342ce9e5330d9d2d7';
const derivativeVersion = 1;
const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(scriptDirectory, '../..');
const fixtureDirectory = join(repositoryRoot, 'target', 'demo-fixtures');
const archivePath = join(fixtureDirectory, 'BigBuckBunny_640x360.m4v.zip');
const sourcePath = join(fixtureDirectory, 'BigBuckBunny_640x360.m4v');
const outputPath = join(fixtureDirectory, 'big-buck-bunny-640x360-h264.mp4');
const manifestPath = join(fixtureDirectory, 'big-buck-bunny-640x360-h264.json');
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
	}>;
	format: { duration?: string; size?: string };
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
	outputFile: string;
	outputSha256: string;
	bytes: number;
	durationSeconds: number;
	codec: 'h264';
	profile: 'Constrained Baseline';
	width: 640;
	height: 360;
	fps: 15;
	blackIntervals: BlackInterval[];
};

type BlackInterval = {
	startSeconds: number;
	endSeconds: number;
};

await mkdir(fixtureDirectory, { recursive: true });
if (!force && (await cachedFixture())) process.exit(0);

if (!(await matchesHash(archivePath, sourceArchiveSha256))) {
	const temporaryArchive = `${archivePath}.download`;
	await rm(temporaryArchive, { force: true });
	const response = await fetch(sourceUrl);
	if (!response.ok) {
		throw new Error(`Big Buck Bunny download failed: ${response.status} ${response.statusText}`);
	}
	await writeFile(temporaryArchive, new Uint8Array(await response.arrayBuffer()));
	const downloadedHash = await sha256(temporaryArchive);
	if (downloadedHash !== sourceArchiveSha256) {
		await rm(temporaryArchive, { force: true });
		throw new Error(
			`Big Buck Bunny archive SHA-256 ${downloadedHash} does not match ${sourceArchiveSha256}`
		);
	}
	await rm(archivePath, { force: true });
	await rename(temporaryArchive, archivePath);
}

await runProcess('unzip', ['-o', archivePath, '-d', fixtureDirectory]);
const extractedHash = await sha256(sourcePath);
if (extractedHash !== sourceMediaSha256) {
	throw new Error(
		`Big Buck Bunny media SHA-256 ${extractedHash} does not match ${sourceMediaSha256}`
	);
}

const temporaryOutput = `${outputPath}.tmp.mp4`;
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
	'scale=640:360:flags=lanczos,fps=15',
	'-an',
	'-c:v',
	'libx264',
	'-preset',
	'veryfast',
	'-crf',
	'28',
	'-profile:v',
	'baseline',
	'-level:v',
	'3.1',
	'-pix_fmt',
	'yuv420p',
	'-g',
	'15',
	'-keyint_min',
	'15',
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
const probe = await validateFixture(temporaryOutput);
const durationSeconds = Number(probe.format.duration);
const blackIntervals = await detectBlackIntervals(temporaryOutput, durationSeconds);
await rm(outputPath, { force: true });
await rename(temporaryOutput, outputPath);
await rm(sourcePath, { force: true });

const outputStats = await stat(outputPath);
const manifest: FixtureManifest = {
	schemaVersion: 1,
	derivativeVersion,
	title: 'Big Buck Bunny',
	sourceUrl,
	sourceArchiveSha256,
	sourceMediaSha256,
	license: 'Creative Commons Attribution 3.0',
	attribution: '(c) copyright 2008, Blender Foundation / www.bigbuckbunny.org',
	outputFile: outputPath,
	outputSha256: await sha256(outputPath),
	bytes: outputStats.size,
	durationSeconds,
	codec: 'h264',
	profile: 'Constrained Baseline',
	width: 640,
	height: 360,
	fps: 15,
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
			manifest.outputFile !== outputPath ||
			!(await matchesHash(outputPath, manifest.outputSha256))
		) {
			return false;
		}
		const probe = await validateFixture(outputPath);
		const durationSeconds = Number(probe.format.duration);
		const blackIntervals = validBlackIntervals(manifest.blackIntervals, durationSeconds)
			? manifest.blackIntervals
			: await detectBlackIntervals(outputPath, durationSeconds);
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

async function validateFixture(filePath: string): Promise<Probe> {
	const output = await runProcess(
		'ffprobe',
		[
			'-v',
			'error',
			'-show_entries',
			'format=duration,size:stream=codec_name,codec_type,profile,pix_fmt,width,height,r_frame_rate',
			'-of',
			'json',
			filePath
		],
		true
	);
	const probe = JSON.parse(output) as Probe;
	const video = probe.streams[0];
	if (
		probe.streams.length !== 1 ||
		video?.codec_name !== 'h264' ||
		video.codec_type !== 'video' ||
		video.profile !== 'Constrained Baseline' ||
		video.pix_fmt !== 'yuv420p' ||
		video.width !== 640 ||
		video.height !== 360 ||
		video.r_frame_rate !== '15/1' ||
		Number(probe.format.duration) < 590
	) {
		throw new Error(`Unexpected Big Buck Bunny derivative: ${JSON.stringify(probe)}`);
	}
	return probe;
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
			fixture: manifest.outputFile,
			durationSeconds: manifest.durationSeconds,
			bytes: manifest.bytes,
			sha256: manifest.outputSha256
		})
	);
}
