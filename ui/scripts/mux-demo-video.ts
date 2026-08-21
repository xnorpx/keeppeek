import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import {
	assertDemoNarrationManifest,
	createNarratedDemoWebVtt,
	type DemoNarrationManifest
} from '../src/lib/server/storybook/demo-narration';
import {
	assertH264AacVideo,
	createFfprobeDurationArgs,
	createFfprobeStreamsArgs,
	createNarratedDemoPlan,
	createPacedDemoVideoMuxArgs,
	parseFfprobeDurationMs
} from '../src/lib/server/storybook/demo-video';
import {
	type StoryScenarioMetadata,
	validateStoryScenarioMetadata
} from '../src/lib/storybook/demo';

const [metadataArgument, videoArgument, manifestArgument, outputArgument, captionsArgument] =
	process.argv.slice(2);
if (
	metadataArgument === undefined ||
	videoArgument === undefined ||
	manifestArgument === undefined ||
	outputArgument === undefined ||
	captionsArgument === undefined
) {
	throw new Error(
		'Usage: bun run demo:mux -- <scenario.json> <silent.mp4> <narration-manifest.json> <narrated.mp4> <narration.vtt>'
	);
}

const metadataPath = resolve(metadataArgument);
const videoPath = resolve(videoArgument);
const manifestPath = resolve(manifestArgument);
const outputPath = resolve(outputArgument);
const captionsPath = resolve(captionsArgument);

async function probeDurationMs(mediaPath: string): Promise<number> {
	return parseFfprobeDurationMs(
		await runProcess('ffprobe', createFfprobeDurationArgs(mediaPath), true)
	);
}

const metadata = JSON.parse(await readFile(metadataPath, 'utf8')) as StoryScenarioMetadata & {
	recording?: Record<string, unknown>;
};
const issues = validateStoryScenarioMetadata(metadata);
if (issues.length > 0) {
	throw new Error(issues.map((issue) => `${issue.path} ${issue.message}`).join('; '));
}
if (!metadata.demo?.narration) {
	throw new Error(`Scenario ${metadata.storyId} has no narration`);
}

const manifest = JSON.parse(await readFile(manifestPath, 'utf8')) as DemoNarrationManifest;
assertDemoNarrationManifest(metadata, manifest);
const audioDirectory = dirname(manifestPath);
const cues = await Promise.all(
	manifest.cues.map(async (cue) => {
		const audioPath = join(audioDirectory, cue.fileName);
		const contents = await readFile(audioPath);
		if (contents.byteLength !== cue.bytes) throw new Error(`WAV size changed: ${cue.fileName}`);
		if (createHash('sha256').update(contents).digest('hex') !== cue.sha256) {
			throw new Error(`WAV hash changed: ${cue.fileName}`);
		}
		const durationMs = await probeDurationMs(audioPath);
		if (durationMs !== cue.durationMs) throw new Error(`WAV duration changed: ${cue.fileName}`);
		return {
			sourceAtMs: cue.sourceAtMs,
			audioPath,
			audioDurationMs: durationMs,
			pauseAfterMs: cue.pauseAfterMs
		};
	})
);
const sourceDurationMs = await probeDurationMs(videoPath);
if (Math.abs(sourceDurationMs - metadata.demo.durationMs) > 100) {
	throw new Error('Silent source duration does not match the authored demo timeline');
}
const plan = createNarratedDemoPlan(metadata.demo.durationMs, cues);
await writeFile(captionsPath, createNarratedDemoWebVtt(metadata.demo.narration, plan));

await runProcess(
	'ffmpeg',
	createPacedDemoVideoMuxArgs({
		videoPath,
		outputPath,
		sourceDurationMs: metadata.demo.durationMs,
		cues
	})
);
const outputDurationMs = await probeDurationMs(outputPath);
if (Math.abs(outputDurationMs - plan.outputDurationMs) > 100) {
	throw new Error('Narrated MP4 duration does not match the audio-led timeline');
}
assertH264AacVideo(
	await runProcess('ffprobe', createFfprobeStreamsArgs(outputPath), true),
	plan.outputDurationMs
);

await writeFile(
	metadataPath,
	`${JSON.stringify(
		{
			...metadata,
			recording: {
				...metadata.recording,
				durationMs: outputDurationMs,
				streamCount: 2,
				audioCodec: 'aac',
				narration: {
					schemaVersion: 1,
					commitSha: manifest.commitSha,
					generatedAt: manifest.generatedAt,
					deployment: manifest.deployment,
					voice: manifest.voice,
					instructions: manifest.instructions,
					speed: manifest.speed,
					cues: manifest.cues.map((cue, index) => ({
						...cue,
						outputStartMs: plan.segments[index]!.outputStartMs,
						freezeDurationMs: plan.segments[index]!.freezeDurationMs
					}))
				}
			}
		},
		null,
		2
	)}\n`
);
console.log(
	JSON.stringify({
		scenarioId: metadata.paper.scenarioId,
		sourceDurationMs,
		outputDurationMs,
		segments: plan.segments
	})
);

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
