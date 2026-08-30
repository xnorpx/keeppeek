import { createHash } from 'node:crypto';
import { readFile, readdir } from 'node:fs/promises';
import { resolve, sep } from 'node:path';
import { isServerCapabilityId } from '../src/lib/capabilities';
import { renderPaperCoverageReport } from './paper-coverage';

type StoryboardBoard = {
	index: number;
	id: string;
	name: string;
	source: string;
	bytes: number;
	sha256: string;
	references?: StoryboardReference[];
};

type StoryboardReference = {
	nodeId: string;
	scenarioId: string;
	storySource: string;
	source: string;
	width: number;
	height: number;
	theme: 'dark' | 'light';
	fixture: string;
	bytes: number;
	sha256: string;
	lokiReference?: {
		source: string;
		bytes: number;
		sha256: string;
		approvedAt: string;
		approvedBy: string;
		paperTokenHash: string;
		paperReferenceSha256: string;
		storybookStoryId: string;
	};
	paperOverlay?: {
		status: 'accepted' | 'candidate';
		reviewedAt: string;
		comparisonThreshold: number;
		mismatchPixels: number;
		totalPixels: number;
		highContrastThreshold: number;
		highContrastMismatchPixels: number;
		varianceReason?: string;
		blockers?: string[];
	};
};

type Storyboard = {
	schemaVersion: number;
	source: {
		artboardCount: number;
		tokenCount: number;
		tokenHash: string;
	};
	boards: StoryboardBoard[];
};

type TokenSnapshot = {
	contentHash: { tokens: string };
	tokens: Array<{ name: string; type: string; value: string }>;
};

type Scenario = {
	id: string;
	boardId: string;
	kind: 'contract' | 'interaction' | 'screen' | 'state';
	route: string | null;
	fixture: string;
	theme: 'both' | 'dark' | 'light' | 'n/a';
	viewport: 'contract' | 'desktop' | 'mobile' | 'responsive';
	requiredCapabilities: string[];
	storybookStoryId: string | null;
	playwrightOwner: string | null;
	contractOwner?: string;
	status: 'contract' | 'planned';
};

type ScenarioManifest = {
	schemaVersion: number;
	paper: { fileId: string; tokenHash: string };
	scenarios: Scenario[];
};

type HandoffManifest = {
	schemaVersion: number;
	boardId: string;
	requirements: Array<{
		index: number;
		name: string;
		scenarioIds: string[];
		status: 'implemented' | 'partial' | 'blocked';
		blockers?: string[];
	}>;
	constraints: string[];
};

type ArchitectureManifest = {
	schemaVersion: number;
	boardId: string;
	desktopDestinations: Array<{
		label: string;
		paperPath: string;
		path: string;
		routeSource: string;
		pathVarianceReason?: string;
		scenarioIds: string[];
		status: 'implemented' | 'partial';
		blockers?: string[];
	}>;
	mobileTabs: string[];
	mobileCameraAccess: { path: string; reason: string };
	settingsSections: string[];
	constraints: string[];
};

type TokenContract = {
	schemaVersion: number;
	boardId: string;
	tokenHash: string;
	tokenCount: number;
	typeCounts: Record<string, number>;
	requiredSemanticTokens: string[];
	minimumFontSizeToken: { name: string; value: string };
	fonts: Array<{
		family: string;
		package: string;
		version: string;
		weights: number[];
	}>;
	constraints: string[];
};

type PositioningContract = {
	schemaVersion: number;
	boardId: string;
	headline: string;
	oneRule: string;
	evidence: Array<{ source: string; contains: string }>;
	eventWireContract: {
		source: string;
		message: string;
		fields: string[];
		persistenceStatus: 'partial';
		blockers: string[];
	};
	httpPaths: string[];
	internalHttpPaths: string[];
	principles: Array<{
		index: number;
		name: string;
		scenarioIds: string[];
		status: 'implemented' | 'partial';
		blockers?: string[];
	}>;
	forbiddenRouteFragments: string[];
	constraints: string[];
};

const root = resolve('design/paper/keeppeek-nvr-v34');

async function readJson<T>(filePath: string): Promise<T> {
	return JSON.parse(await readFile(filePath, 'utf8')) as T;
}

function sha256(contents: Uint8Array): string {
	return createHash('sha256').update(contents).digest('hex');
}

function resolveInsideRoot(relativePath: string): string {
	const absolutePath = resolve(root, relativePath);
	if (!absolutePath.startsWith(`${root}${sep}`)) {
		throw new Error(`Storyboard path escapes export root: ${relativePath}`);
	}
	return absolutePath;
}

function parseCustomProperties(css: string): Map<string, string> {
	return new Map(
		[...css.matchAll(/^\s*(--[a-z0-9-]+):\s*(.+);\s*$/gim)].map((match) => [
			match[1],
			match[2].trim()
		])
	);
}

function normalizedTokenValue(value: string): string {
	return /^#[0-9a-f]+$/i.test(value) ? value.toLowerCase() : value;
}

function verifyTokenValues(
	label: string,
	properties: Map<string, string>,
	tokens: TokenSnapshot['tokens']
): void {
	if (properties.size !== tokens.length) {
		throw new Error(`${label} token count does not match tokens.json`);
	}
	for (const token of tokens) {
		const value = properties.get(token.name);
		if (value === undefined || normalizedTokenValue(value) !== normalizedTokenValue(token.value)) {
			throw new Error(`${label} value does not match tokens.json for ${token.name}`);
		}
	}
}

const storyboard = await readJson<Storyboard>(resolve(root, 'storyboard.json'));
const tokenSnapshot = await readJson<TokenSnapshot>(resolve(root, 'tokens.json'));
const scenarioManifest = await readJson<ScenarioManifest>(resolve(root, 'scenarios.json'));
const handoffManifest = await readJson<HandoffManifest>(resolve(root, 'handoff.json'));
const architectureManifest = await readJson<ArchitectureManifest>(
	resolve(root, 'architecture.json')
);
const tokenContract = await readJson<TokenContract>(resolve(root, 'token-contract.json'));
const positioningContract = await readJson<PositioningContract>(resolve(root, 'positioning.json'));

if (storyboard.schemaVersion !== 2) throw new Error('Unsupported storyboard schema version');
if (storyboard.boards.length !== storyboard.source.artboardCount) {
	throw new Error('Storyboard board count does not match the Paper source');
}
if (tokenSnapshot.contentHash.tokens !== storyboard.source.tokenHash) {
	throw new Error('Storyboard token hash does not match tokens.json');
}
if (tokenSnapshot.tokens.length !== storyboard.source.tokenCount) {
	throw new Error('Storyboard token count does not match tokens.json');
}
if (
	tokenContract.schemaVersion !== 1 ||
	tokenContract.boardId !== '20-0' ||
	tokenContract.tokenHash !== storyboard.source.tokenHash ||
	tokenContract.tokenCount !== tokenSnapshot.tokens.length ||
	tokenContract.constraints.length === 0
) {
	throw new Error('Unsupported Board 02 token contract');
}
const tokenTypeCounts = new Map<string, number>();
const tokensByName = new Map(tokenSnapshot.tokens.map((token) => [token.name, token] as const));
for (const token of tokenSnapshot.tokens) {
	tokenTypeCounts.set(token.type, (tokenTypeCounts.get(token.type) ?? 0) + 1);
}
for (const [type, count] of Object.entries(tokenContract.typeCounts)) {
	if (tokenTypeCounts.get(type) !== count) {
		throw new Error(`Board 02 ${type} token count changed`);
	}
}
if (tokenTypeCounts.size !== Object.keys(tokenContract.typeCounts).length) {
	throw new Error('Board 02 token type set changed');
}
for (const tokenName of tokenContract.requiredSemanticTokens) {
	if (!tokensByName.has(tokenName)) {
		throw new Error(`Board 02 semantic token is missing: ${tokenName}`);
	}
}
if (
	tokensByName.get(tokenContract.minimumFontSizeToken.name)?.value !==
	tokenContract.minimumFontSizeToken.value
) {
	throw new Error('Board 02 minimum font-size token changed');
}
const packageManifest = await readJson<{ devDependencies: Record<string, string> }>(
	resolve('package.json')
);
const appCss = await readFile(resolve('src/app.css'), 'utf8');
for (const font of tokenContract.fonts) {
	const installedManifest = await readJson<{ version: string }>(
		resolve('node_modules', font.package, 'package.json')
	);
	if (
		packageManifest.devDependencies[font.package] !== font.version ||
		installedManifest.version !== font.version
	) {
		throw new Error(`Board 02 font package version drifted: ${font.family}`);
	}
	for (const weight of font.weights) {
		if (!appCss.includes(`@import '${font.package}/latin-${weight}.css';`)) {
			throw new Error(`Board 02 local font weight is missing: ${font.family} ${weight}`);
		}
	}
}
if (scenarioManifest.schemaVersion !== 1) {
	throw new Error('Unsupported scenario manifest schema version');
}
if (scenarioManifest.paper.tokenHash !== storyboard.source.tokenHash) {
	throw new Error('Scenario manifest token hash does not match storyboard.json');
}

const boardIds = new Set<string>();
const boardSources = new Set<string>();
const referenceSources = new Set<string>();
const referenceScenarioIds = new Set<string>();
const references: Array<{ board: StoryboardBoard; reference: StoryboardReference }> = [];
let referenceCount = 0;
for (const [position, board] of storyboard.boards.entries()) {
	if (board.index !== position + 1) throw new Error(`Unexpected board index for ${board.name}`);
	if (boardIds.has(board.id)) throw new Error(`Duplicate Paper board ID: ${board.id}`);
	if (boardSources.has(board.source))
		throw new Error(`Duplicate Paper board source: ${board.source}`);
	boardIds.add(board.id);
	boardSources.add(board.source);

	const contents = await readFile(resolveInsideRoot(board.source));
	if (contents.byteLength !== board.bytes) throw new Error(`Byte count changed for ${board.name}`);
	if (sha256(contents) !== board.sha256) throw new Error(`SHA-256 changed for ${board.name}`);

	for (const reference of board.references ?? []) {
		if (referenceSources.has(reference.source)) {
			throw new Error(`Duplicate Paper reference source: ${reference.source}`);
		}
		if (referenceScenarioIds.has(reference.scenarioId)) {
			throw new Error(`Duplicate Paper reference scenario: ${reference.scenarioId}`);
		}
		if (
			reference.nodeId.trim().length === 0 ||
			reference.fixture.trim().length === 0 ||
			reference.storySource.trim().length === 0
		) {
			throw new Error(`Incomplete Paper reference metadata for ${board.name}`);
		}
		if (reference.width <= 0 || reference.height <= 0) {
			throw new Error(`Invalid Paper reference viewport for ${board.name}`);
		}
		referenceSources.add(reference.source);
		referenceScenarioIds.add(reference.scenarioId);
		references.push({ board, reference });
		referenceCount += 1;
		const referenceContents = await readFile(resolveInsideRoot(reference.source));
		if (referenceContents.byteLength !== reference.bytes) {
			throw new Error(`Reference byte count changed for ${reference.scenarioId}`);
		}
		if (sha256(referenceContents) !== reference.sha256) {
			throw new Error(`Reference SHA-256 changed for ${reference.scenarioId}`);
		}
	}
}

const scenarioIds = new Set<string>();
const coveredBoardIds = new Set<string>();
for (const scenario of scenarioManifest.scenarios) {
	if (scenarioIds.has(scenario.id)) throw new Error(`Duplicate scenario ID: ${scenario.id}`);
	if (!boardIds.has(scenario.boardId)) {
		throw new Error(`Unknown Paper board ID for scenario ${scenario.id}: ${scenario.boardId}`);
	}
	scenarioIds.add(scenario.id);
	coveredBoardIds.add(scenario.boardId);

	if (scenario.fixture.trim().length === 0) throw new Error(`Missing fixture for ${scenario.id}`);
	for (const capability of scenario.requiredCapabilities) {
		if (!isServerCapabilityId(capability)) {
			throw new Error(`Unknown capability for ${scenario.id}: ${capability}`);
		}
	}
	if (scenario.status === 'planned') {
		if (scenario.route === null)
			throw new Error(`Missing route for planned scenario ${scenario.id}`);
		if (scenario.storybookStoryId === null) {
			throw new Error(`Missing Storybook owner for planned scenario ${scenario.id}`);
		}
		if (scenario.playwrightOwner === null) {
			throw new Error(`Missing Playwright owner for planned scenario ${scenario.id}`);
		}
		if (!referenceScenarioIds.has(scenario.id)) {
			throw new Error(`Rendered scenario has no Paper reference: ${scenario.id}`);
		}
	} else {
		if (scenario.route !== null || scenario.storybookStoryId !== null) {
			throw new Error(`Contract scenario must not own a route or story: ${scenario.id}`);
		}
		if (!scenario.contractOwner?.trim()) {
			throw new Error(`Missing contract owner for ${scenario.id}`);
		}
		await readFile(resolve(scenario.contractOwner));
	}
}
for (const boardId of boardIds) {
	if (!coveredBoardIds.has(boardId))
		throw new Error(`Paper board has no scenario coverage: ${boardId}`);
}
for (const scenarioId of referenceScenarioIds) {
	const scenario = scenarioManifest.scenarios.find((candidate) => candidate.id === scenarioId);
	if (!scenario) throw new Error(`Paper reference has no scenario: ${scenarioId}`);
	if (scenario.status !== 'planned') {
		throw new Error(`Contract scenario must not own a Paper raster: ${scenarioId}`);
	}
}
const scenariosById = new Map(
	scenarioManifest.scenarios.map((scenario) => [scenario.id, scenario] as const)
);

if (
	positioningContract.schemaVersion !== 1 ||
	positioningContract.boardId !== '1-0' ||
	!positioningContract.headline.trim() ||
	!positioningContract.oneRule.trim() ||
	positioningContract.constraints.length === 0
) {
	throw new Error('Unsupported Board 01 positioning contract');
}
for (const evidence of positioningContract.evidence) {
	const contents = await readFile(resolve('..', evidence.source), 'utf8');
	if (!contents.includes(evidence.contains)) {
		throw new Error(`Board 01 evidence drifted: ${evidence.source}`);
	}
}
const eventProto = await readFile(
	resolve('..', positioningContract.eventWireContract.source),
	'utf8'
);
const eventMatch = eventProto.match(
	new RegExp(`message\\s+${positioningContract.eventWireContract.message}\\s*\\{([\\s\\S]*?)\\n\\}`)
);
if (!eventMatch) throw new Error('Board 01 Event wire message is missing');
for (const field of positioningContract.eventWireContract.fields) {
	if (!new RegExp(`\\b${field}\\s*=\\s*\\d+;`).test(eventMatch[1])) {
		throw new Error(`Board 01 Event wire field is missing: ${field}`);
	}
}
if (
	positioningContract.eventWireContract.persistenceStatus !== 'partial' ||
	positioningContract.eventWireContract.blockers.length === 0
) {
	throw new Error('Board 01 stored-event limitations are missing');
}
const expectedHttpPaths = ['/create', '/delete', '/logs', '/logs/snapshot', '/metrics'];
const expectedInternalHttpPaths = ['/recording-coverage'];
if (positioningContract.httpPaths.join('|') !== expectedHttpPaths.join('|')) {
	throw new Error('Board 01 HTTP boundary changed');
}
if (positioningContract.internalHttpPaths.join('|') !== expectedInternalHttpPaths.join('|')) {
	throw new Error('Board 45 first-party HTTP boundary changed');
}
const openApi = await readFile(resolve('..', 'api/openapi.yaml'), 'utf8');
const openApiPaths = [...openApi.matchAll(/^  (\/[a-z-]+(?:\/[a-z-]+)*):$/gm)].map(
	(match) => match[1]
);
if (openApiPaths.join('|') !== expectedHttpPaths.join('|')) {
	throw new Error('OpenAPI exposes a noncanonical Board 01 HTTP path');
}
const serverSource = await readFile(resolve('..', 'src/server.rs'), 'utf8');
const serverPaths = [
	...new Set(
		[...serverSource.matchAll(/\((?:GET|POST|OPTIONS)\) \((\/[a-z-]+(?:\/[a-z-]+)*)\)/g)].map(
			(match) => match[1]
		)
	)
];
if (serverPaths.join('|') !== [...expectedHttpPaths, ...expectedInternalHttpPaths].join('|')) {
	throw new Error('Rust router exposes a noncanonical Board 01 HTTP path');
}
const expectedPrinciples = [
	'Footage first',
	'Time is the index',
	'Never lie about state',
	'Test before save',
	'Deep-linkable'
];
if (positioningContract.principles.length !== expectedPrinciples.length) {
	throw new Error('Board 01 must retain exactly five principles');
}
for (const [position, principle] of positioningContract.principles.entries()) {
	if (principle.index !== position + 1 || principle.name !== expectedPrinciples[position]) {
		throw new Error(`Unexpected Board 01 principle: ${principle.name}`);
	}
	for (const scenarioId of principle.scenarioIds) {
		if (!scenariosById.has(scenarioId)) {
			throw new Error(`Unknown Board 01 principle scenario: ${scenarioId}`);
		}
	}
	if (
		principle.status === 'partial' &&
		(!principle.blockers ||
			principle.blockers.length === 0 ||
			principle.blockers.some((blocker) => blocker.trim().length === 0))
	) {
		throw new Error(`Board 01 partial principle needs blockers: ${principle.name}`);
	}
}
const routeFiles = (await readdir(resolve('src/routes'), { recursive: true })).filter((entry) =>
	entry.endsWith('+page.svelte')
);
for (const fragment of positioningContract.forbiddenRouteFragments) {
	if (routeFiles.some((routeFile) => routeFile.toLocaleLowerCase().includes(fragment))) {
		throw new Error(`Board 01 forbids product route: ${fragment}`);
	}
}

if (handoffManifest.schemaVersion !== 1 || handoffManifest.boardId !== 'II-0') {
	throw new Error('Unsupported Board 05 handoff manifest');
}
if (handoffManifest.requirements.length !== 14) {
	throw new Error('Board 05 handoff must account for exactly 14 original screens');
}
if (handoffManifest.constraints.length === 0) {
	throw new Error('Board 05 handoff constraints are missing');
}
const handoffIndexes = new Set<number>();
for (const [position, requirement] of handoffManifest.requirements.entries()) {
	if (requirement.index !== position + 1 || handoffIndexes.has(requirement.index)) {
		throw new Error(`Unexpected Board 05 handoff index: ${requirement.index}`);
	}
	handoffIndexes.add(requirement.index);
	if (requirement.name.trim().length === 0 || requirement.scenarioIds.length === 0) {
		throw new Error(`Incomplete Board 05 handoff requirement ${requirement.index}`);
	}
	for (const scenarioId of requirement.scenarioIds) {
		if (!scenariosById.has(scenarioId)) {
			throw new Error(`Unknown Board 05 handoff scenario: ${scenarioId}`);
		}
	}
	if (
		requirement.status !== 'implemented' &&
		(!requirement.blockers ||
			requirement.blockers.length === 0 ||
			requirement.blockers.some((blocker) => blocker.trim().length === 0))
	) {
		throw new Error(`Board 05 handoff requirement ${requirement.index} needs blockers`);
	}
}

const expectedDestinations = [
	'Dashboard',
	'Viewer',
	'Keep',
	'Events',
	'Cameras',
	'Health',
	'Settings'
];
if (
	architectureManifest.schemaVersion !== 1 ||
	architectureManifest.boardId !== '5Q-0' ||
	architectureManifest.desktopDestinations.length !== expectedDestinations.length
) {
	throw new Error('Unsupported Board 03 architecture manifest');
}
const architecturePaths = new Set<string>();
for (const [position, destination] of architectureManifest.desktopDestinations.entries()) {
	if (destination.label !== expectedDestinations[position]) {
		throw new Error(`Unexpected Board 03 destination: ${destination.label}`);
	}
	if (architecturePaths.has(destination.path)) {
		throw new Error(`Duplicate Board 03 destination path: ${destination.path}`);
	}
	architecturePaths.add(destination.path);
	await readFile(resolve(destination.routeSource));
	for (const scenarioId of destination.scenarioIds) {
		if (!scenariosById.has(scenarioId)) {
			throw new Error(`Unknown Board 03 scenario: ${scenarioId}`);
		}
	}
	if (destination.paperPath !== destination.path && !destination.pathVarianceReason?.trim()) {
		throw new Error(`Board 03 path variance needs a reason: ${destination.label}`);
	}
	if (
		destination.status === 'partial' &&
		(!destination.blockers ||
			destination.blockers.length === 0 ||
			destination.blockers.some((blocker) => blocker.trim().length === 0))
	) {
		throw new Error(`Board 03 partial destination needs blockers: ${destination.label}`);
	}
}
const expectedMobileTabs = ['Dashboard', 'Viewer', 'Keep', 'Events', 'Health', 'More'];
if (architectureManifest.mobileTabs.join('|') !== expectedMobileTabs.join('|')) {
	throw new Error('Board 03 mobile navigation must retain the six-tab responsive contract');
}
if (
	architectureManifest.mobileCameraAccess.path !== '/cameras' ||
	!architectureManifest.mobileCameraAccess.reason.trim()
) {
	throw new Error('Board 03 must explain responsive Camera access');
}
if (
	architectureManifest.settingsSections.length !== 10 ||
	new Set(architectureManifest.settingsSections).size !== 10 ||
	!architectureManifest.settingsSections.includes('Dashboards') ||
	!architectureManifest.settingsSections.includes('Access') ||
	architectureManifest.settingsSections.includes('Camera defaults') ||
	architectureManifest.constraints.length === 0
) {
	throw new Error('Board 03 Settings architecture is incomplete');
}
const mobileNavigationSource = await readFile(
	resolve('src/lib/components/MobileNavigation.svelte'),
	'utf8'
);
for (const tab of expectedMobileTabs) {
	if (!mobileNavigationSource.includes(`label: '${tab}'`)) {
		throw new Error(`Mobile navigation is missing Board 03 tab: ${tab}`);
	}
}
const mobileSettingsSource = await readFile(resolve('src/lib/mobile-settings.ts'), 'utf8');
for (const section of architectureManifest.settingsSections) {
	if (!mobileSettingsSource.includes(`label: '${section}'`)) {
		throw new Error(`Settings index is missing Board 03 section: ${section}`);
	}
}

for (const { board, reference } of references) {
	const scenario = scenariosById.get(reference.scenarioId);
	if (!scenario) throw new Error(`Paper reference has no scenario: ${reference.scenarioId}`);
	if (scenario.boardId !== board.id) {
		throw new Error(`Paper reference board does not match scenario ${reference.scenarioId}`);
	}
	if (scenario.fixture !== reference.fixture || scenario.theme !== reference.theme) {
		throw new Error(`Paper reference fixture or theme drifted for ${reference.scenarioId}`);
	}
	if (scenario.storybookStoryId === null) {
		throw new Error(`Paper reference has no Storybook owner: ${reference.scenarioId}`);
	}

	const storySource = await readFile(resolve(reference.storySource), 'utf8');
	const requiredStoryMetadata = [
		`boardId: '${board.id}'`,
		`frameId: '${reference.nodeId}'`,
		`scenarioId: '${reference.scenarioId}'`,
		`reference: '${reference.source}'`,
		`referenceSha256: '${reference.sha256}'`
	];
	for (const expectedMetadata of requiredStoryMetadata) {
		if (!storySource.includes(expectedMetadata)) {
			throw new Error(
				`Story ${reference.storySource} is missing ${expectedMetadata} for ${reference.scenarioId}`
			);
		}
	}

	const paperOverlay = reference.paperOverlay;
	if (paperOverlay) {
		if (
			paperOverlay.totalPixels !== reference.width * reference.height ||
			paperOverlay.mismatchPixels < 0 ||
			paperOverlay.mismatchPixels > paperOverlay.totalPixels ||
			paperOverlay.highContrastMismatchPixels < 0 ||
			paperOverlay.highContrastMismatchPixels > paperOverlay.mismatchPixels ||
			paperOverlay.highContrastThreshold <= paperOverlay.comparisonThreshold ||
			!/^\d{4}-\d{2}-\d{2}$/.test(paperOverlay.reviewedAt)
		) {
			throw new Error(`Invalid Paper overlay acceptance for ${reference.scenarioId}`);
		}
		if (
			paperOverlay.status === 'accepted' &&
			paperOverlay.highContrastMismatchPixels / paperOverlay.totalPixels > 0.01 &&
			!paperOverlay.varianceReason?.trim()
		) {
			throw new Error(
				`Paper overlay needs a high-contrast variance reason for ${reference.scenarioId}`
			);
		}
		if (
			paperOverlay.status === 'candidate' &&
			(!paperOverlay.blockers ||
				paperOverlay.blockers.length === 0 ||
				paperOverlay.blockers.some((blocker) => blocker.trim().length === 0))
		) {
			throw new Error(`Paper overlay candidate needs named blockers for ${reference.scenarioId}`);
		}
	}

	const lokiReference = reference.lokiReference;
	if (paperOverlay?.status === 'accepted') {
		if (!lokiReference) {
			throw new Error(`Accepted Paper overlay needs a Loki reference for ${reference.scenarioId}`);
		}
		const configuration = scenario.viewport === 'mobile' ? 'chrome.mobile' : 'chrome.desktop';
		const expectedSource = `visual-harness/.loki/reference/${configuration}/${reference.scenarioId}.png`;
		if (
			lokiReference.source !== expectedSource ||
			lokiReference.paperTokenHash !== storyboard.source.tokenHash ||
			lokiReference.paperReferenceSha256 !== reference.sha256 ||
			lokiReference.storybookStoryId !== scenario.storybookStoryId ||
			lokiReference.approvedBy.trim().length === 0 ||
			!/^\d{4}-\d{2}-\d{2}$/.test(lokiReference.approvedAt)
		) {
			throw new Error(`Invalid Loki approval metadata for ${reference.scenarioId}`);
		}
		const lokiContents = await readFile(resolve(lokiReference.source));
		if (
			lokiContents.byteLength !== lokiReference.bytes ||
			sha256(lokiContents) !== lokiReference.sha256
		) {
			throw new Error(`Loki reference drifted for ${reference.scenarioId}`);
		}
	} else if (lokiReference) {
		throw new Error(`Capability-gated scenario cannot approve Loki: ${reference.scenarioId}`);
	}
}
const actualCss = await readFile(resolve(root, 'tokens.css'), 'utf8');
verifyTokenValues('tokens.css', parseCustomProperties(actualCss), tokenSnapshot.tokens);

const runtimeThemeCss = await readFile(resolve('src/styles/paper-theme.css'), 'utf8');
verifyTokenValues('paper-theme.css', parseCustomProperties(runtimeThemeCss), tokenSnapshot.tokens);

const committedCoverageReport = await readFile(resolve(root, 'COVERAGE.md'), 'utf8');
const expectedCoverageReport = await renderPaperCoverageReport(resolve('.'));
if (committedCoverageReport !== expectedCoverageReport) {
	throw new Error('Paper coverage report is stale; run bun run paper:coverage');
}

console.log(
	`Paper storyboard verified: ${storyboard.boards.length} boards, ${tokenSnapshot.tokens.length} tokens, ${scenarioManifest.scenarios.length} scenarios, ${referenceCount} references, ${handoffManifest.requirements.length} handoff requirements, ${architectureManifest.desktopDestinations.length} destinations, ${tokenContract.fonts.length} font families, ${positioningContract.principles.length} principles`
);
