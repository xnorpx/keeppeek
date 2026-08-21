import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname } from 'node:path';
import {
	loadAzureOpenAiTtsConfig,
	synthesizeAzureOpenAiNarration
} from '../src/lib/server/storybook/azure-openai-tts';
import {
	type StoryScenarioMetadata,
	validateStoryScenarioMetadata
} from '../src/lib/storybook/demo';

const [metadataPath, outputPath] = process.argv.slice(2);
if (metadataPath === undefined || outputPath === undefined) {
	throw new Error('Usage: bun run demo:narrate -- <scenario.json> <narration.wav>');
}

const metadata = JSON.parse(await readFile(metadataPath, 'utf8')) as StoryScenarioMetadata;
const issues = validateStoryScenarioMetadata(metadata);
if (issues.length > 0) {
	throw new Error(issues.map((issue) => `${issue.path} ${issue.message}`).join('; '));
}
if (metadata.demo?.narration === undefined) {
	throw new Error(`Scenario ${metadata.storyId} has no narration`);
}

const config = loadAzureOpenAiTtsConfig(process.env);
const audio = await synthesizeAzureOpenAiNarration(metadata.demo.narration, config);
await mkdir(dirname(outputPath), { recursive: true });
await writeFile(outputPath, Buffer.from(audio));
console.log(`Wrote Azure OpenAI narration for ${metadata.storyId} to ${outputPath}`);
