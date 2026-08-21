import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { basename, dirname, extname, join, resolve } from 'node:path';
import {
	createDemoVideoManifest,
	type DemoPublishAsset,
	type DemoPublishEntry
} from '../src/lib/server/storybook/video-publish';
import {
	type StoryScenarioMetadata,
	validateStoryScenarioMetadata
} from '../src/lib/storybook/demo';

const [assetsArgument, baseUrl, outputArgument] = process.argv.slice(2);
if (assetsArgument === undefined || baseUrl === undefined || outputArgument === undefined) {
	throw new Error(
		'Usage: bun run demo:publish:prepare -- <assets-directory> <https-base-url> <manifest.json>'
	);
}

const assetsDirectory = resolve(assetsArgument);
const outputPath = resolve(outputArgument);
const fileNames = await readdir(assetsDirectory);
const metadataFiles = fileNames.filter((fileName) => extname(fileName) === '.json').toSorted();

async function asset(fileName: string): Promise<DemoPublishAsset> {
	return { fileName, contents: await readFile(join(assetsDirectory, fileName)) };
}

const entries: DemoPublishEntry[] = [];
for (const metadataFile of metadataFiles) {
	const scenarioStem = basename(metadataFile, '.json');
	const metadata = JSON.parse(
		await readFile(join(assetsDirectory, metadataFile), 'utf8')
	) as StoryScenarioMetadata;
	const issues = validateStoryScenarioMetadata(metadata);
	if (issues.length > 0) {
		throw new Error(issues.map((issue) => `${issue.path} ${issue.message}`).join('; '));
	}
	if (metadata.paper.scenarioId !== scenarioStem) {
		throw new Error(`Metadata scenario ID does not match filename: ${metadataFile}`);
	}
	entries.push({
		metadata,
		video: await asset(`${scenarioStem}.mp4`),
		captions: await asset(`${scenarioStem}.vtt`),
		metadataAsset: await asset(metadataFile)
	});
}

if (entries.length === 0) throw new Error('No generated demo metadata files found');

const manifest = createDemoVideoManifest({
	baseUrl,
	commitSha: process.env.GITHUB_SHA ?? 'local',
	generatedAt: new Date().toISOString(),
	entries
});
await mkdir(dirname(outputPath), { recursive: true });
await writeFile(outputPath, `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`Prepared ${manifest.videos.length} hosted demo videos at ${outputPath}`);
