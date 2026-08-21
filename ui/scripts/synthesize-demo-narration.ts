import { createHash } from 'node:crypto';
import { execFileSync, spawn } from 'node:child_process';
import { mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import {
	loadAzureOpenAiTtsConfig,
	synthesizeAzureOpenAiNarration
} from '../src/lib/server/storybook/azure-openai-tts';
import type {
	DemoNarrationManifest,
	DemoNarrationManifestCue
} from '../src/lib/server/storybook/demo-narration';
import {
	createFfprobeDurationArgs,
	parseFfprobeDurationMs
} from '../src/lib/server/storybook/demo-video';
import {
	type StoryScenarioMetadata,
	validateStoryScenarioMetadata
} from '../src/lib/storybook/demo';

const [metadataArgument, outputArgument] = process.argv.slice(2);
if (metadataArgument === undefined || outputArgument === undefined) {
	throw new Error('Usage: bun run demo:narrate -- <scenario.json> <output-directory>');
}

const metadataPath = resolve(metadataArgument);
const outputDirectory = resolve(outputArgument);

const metadata = JSON.parse(await readFile(metadataPath, 'utf8')) as StoryScenarioMetadata;
const issues = validateStoryScenarioMetadata(metadata);
if (issues.length > 0) {
	throw new Error(issues.map((issue) => `${issue.path} ${issue.message}`).join('; '));
}
if (metadata.demo?.narration === undefined) {
	throw new Error(`Scenario ${metadata.storyId} has no narration`);
}

const config = loadAzureOpenAiTtsConfig(process.env);
await mkdir(outputDirectory, { recursive: true });
for (const fileName of await readdir(outputDirectory)) {
	if (fileName.endsWith('.wav') || fileName === 'manifest.json') {
		await rm(join(outputDirectory, fileName), { force: true });
	}
}

const cues: DemoNarrationManifestCue[] = [];
for (const [index, cue] of metadata.demo.narration.cues.entries()) {
	const audio = Buffer.from(
		await synthesizeAzureOpenAiNarration(
			{
				text: cue.text,
				voice: metadata.demo.narration.voice,
				...(metadata.demo.narration.instructions === undefined
					? {}
					: { instructions: metadata.demo.narration.instructions }),
				...(metadata.demo.narration.speed === undefined
					? {}
					: { speed: metadata.demo.narration.speed })
			},
			config
		)
	);
	const fileName = `${(index + 1).toString().padStart(3, '0')}.wav`;
	const audioPath = join(outputDirectory, fileName);
	const streamedAudioPath = join(outputDirectory, `${fileName}.stream`);
	await writeFile(streamedAudioPath, audio);
	await normalizeWav(streamedAudioPath, audioPath);
	await rm(streamedAudioPath, { force: true });
	const normalizedAudio = await readFile(audioPath);
	cues.push({
		index,
		sourceAtMs: cue.atMs,
		pauseAfterMs: cue.pauseAfterMs ?? 0,
		text: cue.text,
		fileName,
		durationMs: await probeDurationMs(audioPath),
		bytes: normalizedAudio.byteLength,
		sha256: createHash('sha256').update(normalizedAudio).digest('hex')
	});
}

const manifest: DemoNarrationManifest = {
	schemaVersion: 1,
	storyId: metadata.storyId,
	scenarioId: metadata.paper.scenarioId,
	commitSha: commitSha(),
	generatedAt: new Date().toISOString(),
	deployment: config.deployment,
	voice: metadata.demo.narration.voice,
	...(metadata.demo.narration.instructions === undefined
		? {}
		: { instructions: metadata.demo.narration.instructions }),
	...(metadata.demo.narration.speed === undefined ? {} : { speed: metadata.demo.narration.speed }),
	cues
};
await writeFile(join(outputDirectory, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`Wrote ${cues.length} Azure OpenAI WAV cue(s) for ${metadata.storyId}`);

async function probeDurationMs(mediaPath: string): Promise<number> {
	return new Promise((resolveProbe, rejectProbe) => {
		const processHandle = spawn('ffprobe', createFfprobeDurationArgs(mediaPath), {
			stdio: ['ignore', 'pipe', 'pipe']
		});
		let stdout = '';
		let stderr = '';
		processHandle.stdout.setEncoding('utf8');
		processHandle.stderr.setEncoding('utf8');
		processHandle.stdout.on('data', (chunk: string) => (stdout += chunk));
		processHandle.stderr.on('data', (chunk: string) => (stderr += chunk));
		processHandle.on('error', rejectProbe);
		processHandle.on('close', (exitCode) => {
			if (exitCode !== 0) {
				rejectProbe(new Error(`ffprobe failed: ${stderr.trim()}`));
				return;
			}
			resolveProbe(parseFfprobeDurationMs(stdout));
		});
	});
}

async function normalizeWav(inputPath: string, outputPath: string): Promise<void> {
	await new Promise<void>((resolveProcess, rejectProcess) => {
		const processHandle = spawn(
			'ffmpeg',
			['-v', 'error', '-y', '-i', inputPath, '-c:a', 'pcm_s16le', outputPath],
			{ stdio: ['ignore', 'ignore', 'pipe'] }
		);
		let stderr = '';
		processHandle.stderr.setEncoding('utf8');
		processHandle.stderr.on('data', (chunk: string) => (stderr += chunk));
		processHandle.on('error', rejectProcess);
		processHandle.on('close', (exitCode) => {
			if (exitCode !== 0) {
				rejectProcess(new Error(`ffmpeg WAV normalization failed: ${stderr.trim()}`));
				return;
			}
			resolveProcess();
		});
	});
}

function commitSha(): string {
	if (process.env.GITHUB_SHA?.trim()) return process.env.GITHUB_SHA.trim();
	try {
		return execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
	} catch {
		return 'local';
	}
}
