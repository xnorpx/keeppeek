import { readFile } from 'node:fs/promises';
import {
	assertDemoMediaFits,
	createDemoVideoMuxArgs,
	createFfprobeDurationArgs,
	parseFfprobeDurationMs
} from '../src/lib/server/storybook/demo-video';
import {
	type StoryScenarioMetadata,
	validateStoryScenarioMetadata
} from '../src/lib/storybook/demo';

const [metadataPath, videoPath, audioPath, captionsPath, outputPath, recordingPreRollValue] =
	process.argv.slice(2);
if (
	metadataPath === undefined ||
	videoPath === undefined ||
	audioPath === undefined ||
	captionsPath === undefined ||
	outputPath === undefined ||
	recordingPreRollValue === undefined
) {
	throw new Error(
		'Usage: bun run demo:mux -- <scenario.json> <capture.webm> <narration.wav> <captions.vtt> <demo.mp4> <recording-pre-roll-ms>'
	);
}

async function probeDurationMs(mediaPath: string): Promise<number> {
	const processHandle = Bun.spawn(['ffprobe', ...createFfprobeDurationArgs(mediaPath)], {
		stdout: 'pipe',
		stderr: 'pipe'
	});
	const [stdout, stderr, exitCode] = await Promise.all([
		new Response(processHandle.stdout).text(),
		new Response(processHandle.stderr).text(),
		processHandle.exited
	]);
	if (exitCode !== 0) {
		throw new Error(`ffprobe failed for ${mediaPath}: ${stderr.trim()}`);
	}
	return parseFfprobeDurationMs(stdout);
}

const metadata = JSON.parse(await readFile(metadataPath, 'utf8')) as StoryScenarioMetadata;
const issues = validateStoryScenarioMetadata(metadata);
if (issues.length > 0) {
	throw new Error(issues.map((issue) => `${issue.path} ${issue.message}`).join('; '));
}
if (metadata.demo?.narration === undefined) {
	throw new Error(`Scenario ${metadata.storyId} has no narration`);
}

const recordingPreRollMs = Number(recordingPreRollValue);
const audioDelayMs = metadata.demo.narration.startAtMs ?? 0;
const [videoDurationMs, narrationDurationMs] = await Promise.all([
	probeDurationMs(videoPath),
	probeDurationMs(audioPath)
]);
assertDemoMediaFits({
	demoDurationMs: metadata.demo.durationMs,
	videoDurationMs,
	recordingPreRollMs,
	narrationDurationMs,
	audioDelayMs
});

const processHandle = Bun.spawn(
	[
		'ffmpeg',
		...createDemoVideoMuxArgs({
			videoPath,
			audioPath,
			captionsPath,
			outputPath,
			durationMs: metadata.demo.durationMs,
			recordingPreRollMs,
			audioDelayMs
		})
	],
	{ stdout: 'inherit', stderr: 'inherit' }
);
const exitCode = await processHandle.exited;
if (exitCode !== 0) throw new Error(`ffmpeg exited with code ${exitCode}`);
