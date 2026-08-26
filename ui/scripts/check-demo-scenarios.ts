import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { cameraLifecycleStory } from '../demo/camera-lifecycle.story';
import { nineCameraLiveStory } from '../demo/nine-camera-live.story';
import { demoAssetId, validateStoryScenarioMetadata } from '../src/lib/storybook/demo';
import { demoScenarios } from '../visual-harness/demo-scenarios';

type ScenarioManifest = {
	scenarios: Array<{ id: string; kind: string; storybookStoryId: string | null }>;
};

const manifest = JSON.parse(
	await readFile(resolve('design/paper/keeppeek-nvr-v34/scenarios.json'), 'utf8')
) as ScenarioManifest;
const scenariosById = new Map(manifest.scenarios.map((scenario) => [scenario.id, scenario]));
const storyIds = new Set<string>();
const scenarioIds = new Set<string>();
const assetIds = new Set<string>();

for (const definition of demoScenarios) {
	const issues = validateStoryScenarioMetadata(definition.metadata);
	if (issues.length > 0) {
		throw new Error(
			`${definition.metadata.storyId}: ${issues.map((issue) => `${issue.path} ${issue.message}`).join('; ')}`
		);
	}
	if (storyIds.has(definition.metadata.storyId)) {
		throw new Error(`Duplicate demo story ID: ${definition.metadata.storyId}`);
	}
	storyIds.add(definition.metadata.storyId);
	const scenarioId = definition.metadata.paper.scenarioId;
	const assetId = demoAssetId(definition.metadata);
	if (scenarioIds.has(scenarioId)) throw new Error(`Duplicate demo scenario ID: ${scenarioId}`);
	scenarioIds.add(scenarioId);
	if (assetIds.has(assetId)) throw new Error(`Duplicate demo asset ID: ${assetId}`);
	assetIds.add(assetId);
	const manifestScenario = scenariosById.get(scenarioId);
	if (!manifestScenario) throw new Error(`Demo has no Paper scenario: ${scenarioId}`);
	if (manifestScenario.kind !== 'interaction') {
		throw new Error(`Demo scenario must be an interaction: ${scenarioId}`);
	}
	if (definition.previewScenarioId !== scenarioId) {
		throw new Error(`Demo preview and Paper scenario must match: ${scenarioId}`);
	}
	if (
		definition.metadata.demo!.viewport.width % 2 ||
		definition.metadata.demo!.viewport.height % 2
	) {
		throw new Error(`Demo viewport must use even H.264 dimensions: ${scenarioId}`);
	}
	const storySource = await readFile(resolve(definition.storySource), 'utf8');
	if (!storySource.includes('metadata.demo') || !storySource.includes('metadata.paper')) {
		throw new Error(`Story does not expose demo and Paper metadata: ${definition.storySource}`);
	}
	for (const fixtureSource of definition.fixtureSources) {
		await readFile(resolve(fixtureSource));
	}
}

const lifecycleIssues = validateStoryScenarioMetadata(cameraLifecycleStory);
if (lifecycleIssues.length > 0) {
	throw new Error(
		`${cameraLifecycleStory.storyId}: ${lifecycleIssues.map((issue) => `${issue.path} ${issue.message}`).join('; ')}`
	);
}
const lifecycleScenario = scenariosById.get(cameraLifecycleStory.paper.scenarioId);
if (!lifecycleScenario) {
	throw new Error(
		`Camera lifecycle story has no Paper scenario: ${cameraLifecycleStory.paper.scenarioId}`
	);
}
if (cameraLifecycleStory.demo.viewport.width % 2 || cameraLifecycleStory.demo.viewport.height % 2) {
	throw new Error('Camera lifecycle viewport must use even H.264 dimensions');
}
for (const source of [
	'demo/camera-lifecycle.story.ts',
	'demo/camera-lifecycle.demo.ts',
	'playwright.demo.config.ts'
]) {
	await readFile(resolve(source));
}
const lifecyclePlaywrightConfig = await readFile(resolve('playwright.demo.config.ts'), 'utf8');
if (!lifecyclePlaywrightConfig.includes("testMatch: '**/camera-lifecycle.demo.ts'")) {
	throw new Error('Camera lifecycle Playwright config must select only its own recorder');
}

const nineCameraIssues = validateStoryScenarioMetadata(nineCameraLiveStory);
if (nineCameraIssues.length > 0) {
	throw new Error(
		`${nineCameraLiveStory.storyId}: ${nineCameraIssues.map((issue) => `${issue.path} ${issue.message}`).join('; ')}`
	);
}
const nineCameraScenario = scenariosById.get(nineCameraLiveStory.paper.scenarioId);
if (!nineCameraScenario) {
	throw new Error(
		`Nine-camera story has no Paper scenario: ${nineCameraLiveStory.paper.scenarioId}`
	);
}
if (nineCameraLiveStory.demo.viewport.width % 2 || nineCameraLiveStory.demo.viewport.height % 2) {
	throw new Error('Nine-camera viewport must use even H.264 dimensions');
}
const nineCameraActions = nineCameraLiveStory.demo.actions;
if (
	nineCameraActions.length !== 3 ||
	nineCameraActions.filter((action) => action.selector === 'a[aria-label="Peek"]').length !== 1 ||
	nineCameraActions.filter(
		(action) => action.selector === '[data-camera-id="192.0.2.101"] button[data-peek-camera-label]'
	).length !== 2
) {
	throw new Error('Nine-camera demo must open Peek and toggle one camera diagnostic twice');
}
for (const source of [
	'demo/nine-camera-live.story.ts',
	'demo/nine-camera-live.demo.ts',
	'playwright.nine-camera-demo.config.ts',
	'scripts/prepare-nine-camera-demo-fixture.ts',
	'scripts/start-nine-camera-demo-server.ts'
]) {
	await readFile(resolve(source));
}
const nineCameraPlaywrightConfig = await readFile(
	resolve('playwright.nine-camera-demo.config.ts'),
	'utf8'
);
if (!nineCameraPlaywrightConfig.includes("testMatch: '**/nine-camera-live.demo.ts'")) {
	throw new Error('Nine-camera Playwright config must select only its own recorder');
}
const nineCameraLauncher = await readFile(
	resolve('scripts/start-nine-camera-demo-server.ts'),
	'utf8'
);
if (
	!nineCameraLauncher.includes('camera-drafts.json') ||
	!nineCameraLauncher.includes('testCameras.map((camera) => camera.config)')
) {
	throw new Error('Nine-camera demo server must retain drafts and start from camera tables');
}

const previewSource = await readFile(resolve('visual-harness/local-preview.ts'), 'utf8');
for (const scenarioId of scenarioIds) {
	if (!previewSource.includes(`scenarioId === '${scenarioId}'`)) {
		throw new Error(`Local preview does not mount demo scenario: ${scenarioId}`);
	}
}
if (
	!previewSource.includes('__keepPeekDemoStart') ||
	!previewSource.includes('dataset.demoReady')
) {
	throw new Error('Local preview must emit the explicit demo-start signal');
}

const packageManifest = JSON.parse(await readFile(resolve('package.json'), 'utf8')) as {
	scripts: Record<string, string>;
};
if (
	packageManifest.scripts['demo:render'] !==
	'bun run demo:render:storybook && bun run demo:render:camera-lifecycle && bun run demo:render:nine-camera'
) {
	throw new Error('demo:render must invoke every canonical recorder');
}
if (
	packageManifest.scripts['demo:gate'] !==
	'bun run demo:typecheck && bun run demo:check && bun run demo:render'
) {
	throw new Error('demo:gate must validate contracts before recording every canonical demo');
}
if (
	packageManifest.scripts['demo:render:camera-lifecycle'] !==
	'bun run test:e2e:prepare && playwright test --config playwright.demo.config.ts'
) {
	throw new Error('Camera lifecycle demo must use its real-server Playwright configuration');
}
if (
	packageManifest.scripts['demo:render:nine-camera'] !==
	'bun run demo:fixtures:prepare && bun run test:e2e:prepare && playwright test --config playwright.nine-camera-demo.config.ts'
) {
	throw new Error(
		'Nine-camera demo must prepare its fixture and use its real-server configuration'
	);
}
if (
	packageManifest.scripts['demo:render:narrated'] !==
	'bun run demo:render && bun run demo:narrate:all'
) {
	throw new Error('Narrated demo rendering must preserve silent sources before Azure synthesis');
}
const generationWorkflow = await readFile(
	resolve('..', '.github/workflows/generate-demo-videos.yml'),
	'utf8'
);
for (const requiredText of [
	'name: Generate Demo Videos',
	'bun run demo:render',
	'bun run demo:narrate:all',
	'AZURE_OPENAI_AUTH_TOKEN',
	'gpt-4o-mini-tts',
	'src/lib/server/storybook/azure-openai-tts.spec.ts',
	'src/lib/server/storybook/demo-narration.spec.ts',
	'bun run demo:publish:prepare',
	"find test-results/demo-videos -name '*.webm'",
	'stream=codec_name,codec_type,pix_fmt',
	'name: keeppeek-demo-videos'
]) {
	if (!generationWorkflow.includes(requiredText)) {
		throw new Error(`Demo generation workflow is missing: ${requiredText}`);
	}
}
const publishingWorkflow = await readFile(
	resolve('..', '.github/workflows/publish-demo-videos.yml'),
	'utf8'
);
for (const requiredText of [
	'workflows: [Generate Demo Videos]',
	'name: keeppeek-demo-videos',
	'path: demo-videos',
	'client-id: ${{ vars.AZURE_CLIENT_ID }}',
	'AZURE_DEMO_BASE_URL'
]) {
	if (!publishingWorkflow.includes(requiredText)) {
		throw new Error(`Demo publishing workflow is missing: ${requiredText}`);
	}
}

const bookGallery = await readFile(resolve('..', 'book/src/demo-videos.md'), 'utf8');

for (const assetId of [
	...assetIds,
	demoAssetId(cameraLifecycleStory),
	demoAssetId(nineCameraLiveStory)
]) {
	if (!bookGallery.includes(`${assetId}.mp4`) || !bookGallery.includes(`${assetId}.vtt`)) {
		throw new Error(`Book demo gallery is missing video or captions for: ${assetId}`);
	}
}

console.log(
	`Demo registry verified: ${demoScenarios.length} interactive Storybook scenario(s), 2 real-server Playwright stories`
);
