import { spawn } from 'node:child_process';
import { copyFile, mkdir, readFile, readdir, rename, rm } from 'node:fs/promises';
import { basename, dirname, join, resolve } from 'node:path';
import type { StoryScenarioMetadata } from '../src/lib/storybook/demo';

const assetsDirectory = resolve(process.argv[2] ?? 'test-results/demo-videos/assets');
const rootDirectory = dirname(assetsDirectory);
const narrationDirectory = join(rootDirectory, 'narration');
const silentDirectory = join(rootDirectory, 'silent');
const renderedDirectory = join(rootDirectory, 'narrated');
await Promise.all(
	[narrationDirectory, silentDirectory, renderedDirectory].map(async (directory) => {
		await rm(directory, { recursive: true, force: true });
		await mkdir(directory, { recursive: true });
	})
);

const metadataFiles = (await readdir(assetsDirectory))
	.filter((fileName) => fileName.endsWith('.json'))
	.toSorted();
if (metadataFiles.length === 0) throw new Error('No demo metadata found for narration');

for (const metadataFile of metadataFiles) {
	const scenarioId = basename(metadataFile, '.json');
	const metadataPath = join(assetsDirectory, metadataFile);
	const metadata = JSON.parse(await readFile(metadataPath, 'utf8')) as StoryScenarioMetadata;
	if (!metadata.demo?.narration) throw new Error(`Scenario ${scenarioId} has no narration cues`);
	const sourceVideoPath = join(assetsDirectory, `${scenarioId}.mp4`);
	const sourceCaptionsPath = join(assetsDirectory, `${scenarioId}.vtt`);
	const silentVideoPath = join(silentDirectory, `${scenarioId}.mp4`);
	const silentCaptionsPath = join(silentDirectory, `${scenarioId}.vtt`);
	const silentMetadataPath = join(silentDirectory, metadataFile);
	const scenarioNarrationDirectory = join(narrationDirectory, scenarioId);
	const narratedVideoPath = join(renderedDirectory, `${scenarioId}.mp4`);
	const narratedCaptionsPath = join(renderedDirectory, `${scenarioId}.vtt`);
	await Promise.all([
		copyFile(sourceVideoPath, silentVideoPath),
		copyFile(sourceCaptionsPath, silentCaptionsPath),
		copyFile(metadataPath, silentMetadataPath)
	]);
	await runBunScript('scripts/synthesize-demo-narration.ts', [
		metadataPath,
		scenarioNarrationDirectory
	]);
	await runBunScript('scripts/mux-demo-video.ts', [
		metadataPath,
		silentVideoPath,
		join(scenarioNarrationDirectory, 'manifest.json'),
		narratedVideoPath,
		narratedCaptionsPath
	]);
	await Promise.all([
		rename(narratedVideoPath, sourceVideoPath),
		rename(narratedCaptionsPath, sourceCaptionsPath)
	]);
}

console.log(`Rendered ${metadataFiles.length} audio-paced narrated demo(s)`);

async function runBunScript(scriptPath: string, args: string[]): Promise<void> {
	await new Promise<void>((resolveProcess, rejectProcess) => {
		const processHandle = spawn(process.execPath, [resolve(scriptPath), ...args], {
			stdio: 'inherit',
			env: process.env
		});
		processHandle.on('error', rejectProcess);
		processHandle.on('close', (exitCode) => {
			if (exitCode !== 0) {
				rejectProcess(new Error(`${scriptPath} exited with code ${exitCode}`));
				return;
			}
			resolveProcess();
		});
	});
}
