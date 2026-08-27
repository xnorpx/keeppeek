import { createRequire } from 'node:module';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';

type Scenario = {
	id: string;
	viewport: 'contract' | 'desktop' | 'mobile' | 'responsive';
};

type ScenarioManifest = {
	scenarios: Scenario[];
};

type Storyboard = {
	boards: Array<{
		references?: Array<{ scenarioId: string; storySource: string }>;
	}>;
};

type LokiConfiguration = {
	storiesFilter: string;
	disableAutomaticViewportHeight: boolean;
	mobile?: boolean;
};

type LokiConfig = {
	skipStories: string;
	pixelmatch: { threshold: number };
	fileNameFormatter: (input: {
		configurationName: string;
		parameters: { paper: { scenarioId: string } };
	}) => string;
	configurations: Record<'chrome.desktop' | 'chrome.mobile', LokiConfiguration>;
};

const designRoot = resolve('design/paper/keeppeek-nvr-v34');
const scenarioManifest = JSON.parse(
	await readFile(resolve(designRoot, 'scenarios.json'), 'utf8')
) as ScenarioManifest;
const storyboard = JSON.parse(
	await readFile(resolve(designRoot, 'storyboard.json'), 'utf8')
) as Storyboard;
const scenariosById = new Map(
	scenarioManifest.scenarios.map((scenario) => [scenario.id, scenario])
);
const require = createRequire(import.meta.url);
const loki = require(resolve('visual-harness/loki.config.cjs')) as LokiConfig;
const packageManifest = JSON.parse(
	await readFile(resolve('visual-harness/package.json'), 'utf8')
) as { scripts: Record<string, string>; devDependencies: Record<string, string> };
const workflow = await readFile(resolve('..', '.github/workflows/visual-regression.yml'), 'utf8');
const localPreviewHtml = await readFile(resolve('visual-harness/local-preview.html'), 'utf8');
const localPreviewCss = await readFile(resolve('visual-harness/local-preview.css'), 'utf8');
const storybookReadiness = await readFile(resolve('visual-harness/storybook-readiness.ts'), 'utf8');
const storybookPreviewEntry = await readFile(
	resolve('visual-harness/.storybook/preview.ts'),
	'utf8'
);

if (
	!localPreviewHtml.includes('data-visual-fixture-notice') ||
	!localPreviewHtml.includes('not measurements from this machine')
) {
	throw new Error('The local visual preview must identify deterministic fixture data.');
}
if (
	!localPreviewCss.match(
		/html\[data-demo='true'\] \[data-visual-fixture-notice\]\s*\{[^}]*display:\s*none;/s
	)
) {
	throw new Error('Demo captures must hide the local fixture-data notice.');
}
if (
	!storybookReadiness.includes('waitForStorybookIndexBeforeExtract') ||
	!storybookReadiness.includes('await storybookPreview.ready()')
) {
	throw new Error('Loki story extraction must wait for the Storybook index to be ready.');
}
if (
	!storybookReadiness.includes('registerPendingPromise') ||
	!storybookReadiness.includes("Object.defineProperty(host, '__STORYBOOK_PREVIEW__'") ||
	!storybookReadiness.includes("Object.defineProperty(preview, 'storyStore'")
) {
	throw new Error(
		'The readiness guard must survive preview assignment order and hold Loki page load until the index is ready.'
	);
}
if (!storybookPreviewEntry.includes('installStorybookReadinessGuard(window')) {
	throw new Error('The Storybook preview must install the Loki readiness guard on window.');
}

for (const skippedStory of ['Foundation/Capability Gate Unsupported', 'Demos/Peek History']) {
	if (!new RegExp(loki.skipStories, 'i').test(skippedStory)) {
		throw new Error(`Loki must exclude the moving story: ${skippedStory}`);
	}
}
for (const [configurationName, configuration] of Object.entries(loki.configurations)) {
	if (configuration.disableAutomaticViewportHeight) {
		throw new Error(`${configurationName} must expand to the authored Paper frame height`);
	}
}
if (!loki.configurations['chrome.mobile'].mobile) {
	throw new Error('Loki mobile capture must enable browser mobile emulation');
}
if (loki.pixelmatch.threshold <= 0 || loki.pixelmatch.threshold > 0.004) {
	throw new Error('Loki pixelmatch threshold must stay within the reviewed antialias ceiling');
}

const references = storyboard.boards.flatMap((board) => board.references ?? []);
for (const reference of references) {
	const scenario = scenariosById.get(reference.scenarioId);
	if (!scenario) throw new Error(`Unknown visual scenario: ${reference.scenarioId}`);
	if (scenario.viewport !== 'desktop' && scenario.viewport !== 'mobile') {
		throw new Error(`Visual scenario needs a desktop or mobile viewport: ${scenario.id}`);
	}
	const storySource = await readFile(resolve(reference.storySource), 'utf8');
	const title = storySource.match(/\btitle:\s*'([^']+)'/)?.[1];
	if (!title) throw new Error(`Story title is missing: ${reference.storySource}`);
	const configurationName = `chrome.${scenario.viewport}` as const;
	const otherConfigurationName =
		scenario.viewport === 'mobile' ? 'chrome.desktop' : 'chrome.mobile';
	if (!new RegExp(loki.configurations[configurationName].storiesFilter, 'i').test(title)) {
		throw new Error(`${scenario.id} is excluded from ${configurationName}`);
	}
	if (new RegExp(loki.configurations[otherConfigurationName].storiesFilter, 'i').test(title)) {
		throw new Error(`${scenario.id} leaks into ${otherConfigurationName}`);
	}
	const filename = loki.fileNameFormatter({
		configurationName,
		parameters: { paper: { scenarioId: scenario.id } }
	});
	if (filename !== `${configurationName}/${scenario.id}`) {
		throw new Error(`Unstable Loki filename for ${scenario.id}: ${filename}`);
	}
}

const ciCommand = packageManifest.scripts['loki:test:ci'];
if (packageManifest.devDependencies['@ferocia-oss/osnap'] !== '1.3.5') {
	throw new Error("The visual harness must pin Loki's eagerly required osnap runtime.");
}
if (
	!ciCommand?.includes("--configurationFilter '^chrome\\.'") ||
	!ciCommand.includes('--requireReference=false') ||
	!ciCommand.includes('env -u CI -u CONTINUOUS_INTEGRATION -u BUILD_NUMBER -u RUN_ID') ||
	!ciCommand.includes('--verboseRenderer')
) {
	throw new Error(
		'Loki CI must compare both Chrome configurations and capture missing references noninteractively'
	);
}
for (const requiredWorkflowText of [
	'run: bun run storybook:build',
	'run: bun run loki:test:ci',
	'ui/visual-harness/.loki/reference',
	'ui/visual-harness/.loki/current',
	'ui/visual-harness/.loki/difference',
	'ui/design/paper/keeppeek-nvr-v34/COVERAGE.md'
]) {
	if (!workflow.includes(requiredWorkflowText)) {
		throw new Error(`Visual workflow is missing: ${requiredWorkflowText}`);
	}
}

console.log(`Visual harness verified: ${references.length} Paper stories with stable Loki paths`);
