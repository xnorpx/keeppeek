import { createHash } from 'node:crypto';
import { readFileSync, realpathSync, statSync } from 'node:fs';
import { dirname, isAbsolute, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { beforeAll, describe, expect, it } from 'vitest';

type Artifact = { source: string; bytes: number; sha256: string };
type Reference = Artifact & {
	scenarioId: string;
	nodeId: string;
	width: number;
	height: number;
	scale: number;
	status: 'reference' | 'historical' | 'proposal';
	supersededBy?: string;
};
type Board = {
	number: number;
	nodeId: string;
	name: string;
	width: number;
	height: number;
	jsx: Artifact;
	references: Reference[];
};
type Manifest = {
	schemaVersion: number;
	exportedAt: string;
	source: {
		fileId: string;
		url: string;
		pageId: string;
		tokenHash: string;
		tokenCount: number;
		artboardCount: number;
	};
	scope: { product: string; boardNumbers: number[]; excludedProducts: string[] };
	tokens: Artifact;
	boards: Board[];
};
type TokenSnapshot = {
	contentHash: { tokens: string };
	tokens: Array<{ name: string; type: string; value: string }>;
};

const uiRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const snapshotRoot = resolve(uiRoot, 'design/paper/keeppeek-nvr-alpha');
const requiredBoards = [...Array.from({ length: 34 }, (_, index) => index + 1), 45, 46];
const contractOnlyBoards = new Set([1, 2, 3, 5, 28, 32]);
const supplementalScenarios = [
	'settings.desktop.storage-setup-reviewed',
	'settings.access.user-states',
	'settings.backup.configuration-zip-states',
	'recordings.desktop.integrity',
	'keep.mobile.compact-default',
	'keep.mobile.playback-options',
	'keep.mobile.camera-date'
];
const maximumArtifactBytes = 64 * 1024 * 1024;
// Concurrent Windows builds can stall disk reads while these tests hash the full export bundle.
const artifactTestTimeoutMs = 30_000;
let manifest: Manifest;

function confinedPath(source: string): string {
	expect(source).not.toBe('');
	expect(isAbsolute(source), source).toBe(false);
	expect(source, 'Export paths must be portable relative paths').not.toMatch(/\\|^[a-z]:/i);
	expect(source.split('/'), source).not.toContain('..');
	const root = realpathSync(snapshotRoot);
	const path = realpathSync(resolve(root, source));
	const local = relative(root, path);
	expect(isAbsolute(local), source).toBe(false);
	expect(local.split(/[\\/]/), source).not.toContain('..');
	return path;
}

function readArtifact(artifact: Artifact): Buffer {
	const path = confinedPath(artifact.source);
	const stat = statSync(path);
	expect(stat.isFile(), artifact.source).toBe(true);
	expect(stat.size, artifact.source).toBeGreaterThan(0);
	expect(stat.size, artifact.source).toBeLessThanOrEqual(maximumArtifactBytes);
	expect(stat.size, artifact.source).toBe(artifact.bytes);
	expect(artifact.sha256, artifact.source).toMatch(/^[a-f0-9]{64}$/);
	const bytes = readFileSync(path);
	expect(createHash('sha256').update(bytes).digest('hex'), artifact.source).toBe(artifact.sha256);
	return bytes;
}

function expectUnique(values: Array<string | number>, label: string): void {
	expect(new Set(values).size, label).toBe(values.length);
}

function expectDimensions(width: number, height: number, label: string): void {
	for (const dimension of [width, height]) {
		expect(Number.isInteger(dimension), label).toBe(true);
		expect(dimension, label).toBeGreaterThan(0);
		expect(dimension, label).toBeLessThanOrEqual(32_768);
	}
}

function verifyPng(reference: Reference): void {
	expect(reference.source).toMatch(/\.png$/);
	expectDimensions(reference.width, reference.height, reference.scenarioId);
	expect(reference.scale).toBe(1);
	const bytes = readArtifact(reference);
	expect(bytes.length, reference.source).toBeGreaterThanOrEqual(33);
	expect(bytes.subarray(0, 8).toString('hex'), reference.source).toBe('89504e470d0a1a0a');
	expect(bytes.subarray(12, 16).toString('ascii'), reference.source).toBe('IHDR');
	expect(bytes.readUInt32BE(16), reference.source).toBe(reference.width);
	expect(bytes.readUInt32BE(20), reference.source).toBe(reference.height);
}

function normalizedTokenValue(value: string): string {
	return /^#[0-9a-f]+$/i.test(value) ? value.toLowerCase() : value;
}

describe('current NVR Paper reference snapshot', () => {
	beforeAll(() => {
		const path = resolve(snapshotRoot, 'manifest.json');
		if (statSync(path).size > 1024 * 1024) throw new Error('Paper manifest exceeds 1 MiB');
		manifest = JSON.parse(readFileSync(path, 'utf8')) as Manifest;
	});

	it('records the source and includes every NVR board without separate products', () => {
		expect(manifest.schemaVersion).toBe(1);
		expect(Number.isFinite(Date.parse(manifest.exportedAt))).toBe(true);
		expect(manifest.source.fileId).toBe('01M0B0VBH78TMTX40GCYYQ37SG');
		expect(manifest.source.url).toBe(`https://app.paper.design/file/${manifest.source.fileId}/1-0`);
		expect(manifest.source.pageId).toBe('1-0');
		expect(manifest.source.tokenHash).toBe('b35ec365');
		expect(manifest.source.tokenCount).toBe(82);
		expect(manifest.source.artboardCount).toBeGreaterThanOrEqual(82);
		expect(manifest.scope.product).toBe('KeepPeek NVR');
		expect(manifest.scope.excludedProducts.length).toBeGreaterThan(0);
		const numbers = manifest.boards.map((board) => board.number).toSorted((a, b) => a - b);
		expectUnique(numbers, 'Duplicate board numbers');
		expect(numbers).toEqual(requiredBoards);
		expect(manifest.scope.boardNumbers.toSorted((a, b) => a - b)).toEqual(numbers);
		expectUnique(
			manifest.boards.map((board) => board.nodeId),
			'Duplicate board node IDs'
		);
		for (const board of manifest.boards) {
			expect(board.nodeId).toMatch(/^[A-Z0-9]+-[0-9]+$/i);
			expect(board.name).not.toMatch(/\biOS\b|\bAndroid\b|\bVision\b|\bKP Service\b/i);
			expectDimensions(board.width, board.height, board.name);
		}
	});

	it('retains all three reviewed mobile Keep states on board 46', () => {
		const board = manifest.boards.find((candidate) => candidate.number === 46);
		expect(board).toBeDefined();
		expect(board!.nodeId).toBe('AW0-0');
		const frames = board!.references
			.map(({ scenarioId, nodeId, status }) => ({ scenarioId, nodeId, status }))
			.toSorted((left, right) => left.scenarioId.localeCompare(right.scenarioId));
		expect(frames).toEqual([
			{ scenarioId: 'keep.mobile.camera-date', nodeId: 'B3V-0', status: 'proposal' },
			{ scenarioId: 'keep.mobile.compact-default', nodeId: 'AW8-0', status: 'proposal' },
			{ scenarioId: 'keep.mobile.playback-options', nodeId: 'B09-0', status: 'proposal' }
		]);
	});

	it(
		'hash-locks every original JSX export and keeps all artifact paths inside the snapshot',
		() => {
			const references = manifest.boards.flatMap((board) => board.references);
			const sources = [
				manifest.tokens.source,
				...manifest.boards.map((board) => board.jsx.source),
				...references.map((reference) => reference.source)
			];
			expectUnique(sources, 'An export path is registered more than once');
			for (const board of manifest.boards) {
				expect(board.jsx.source).toMatch(/\.jsx\.txt$/);
				expect(readArtifact(board.jsx).toString('utf8'), board.name).toContain('<');
			}
			readArtifact(manifest.tokens);
			for (const reference of references) readArtifact(reference);
		},
		artifactTestTimeoutMs
	);

	it(
		'identifies reference states uniquely and verifies their actual PNG dimensions',
		() => {
			const references = manifest.boards.flatMap((board) => board.references);
			const legacy = JSON.parse(
				readFileSync(resolve(uiRoot, 'design/paper/keeppeek-nvr-v34/storyboard.json'), 'utf8')
			) as { boards: Array<{ references?: Array<{ scenarioId: string }> }> };
			const legacyScenarios = legacy.boards.flatMap((board) =>
				(board.references ?? []).map((reference) => reference.scenarioId)
			);
			expect(legacyScenarios).toHaveLength(49);
			expect(references.map((reference) => reference.scenarioId).toSorted()).toEqual(
				[...legacyScenarios, ...supplementalScenarios].toSorted()
			);
			const renderedBoards = manifest.boards.filter(
				(board) => !contractOnlyBoards.has(board.number)
			);
			expect(references.length).toBeGreaterThanOrEqual(renderedBoards.length);
			expect(references.length).toBeLessThanOrEqual(256);
			expectUnique(
				references.map((reference) => reference.scenarioId),
				'Duplicate scenario IDs'
			);
			expectUnique(
				references.map((reference) => reference.nodeId),
				'Duplicate reference node IDs'
			);
			const scenarios = new Set(references.map((reference) => reference.scenarioId));
			for (const board of manifest.boards) {
				if (!contractOnlyBoards.has(board.number)) {
					expect(board.references.length, board.name).toBeGreaterThan(0);
				}
				for (const reference of board.references) {
					expect(reference.scenarioId).not.toBe('');
					expect(reference.nodeId).toMatch(/^[A-Z0-9]+-[0-9]+$/i);
					expect(['reference', 'historical', 'proposal']).toContain(reference.status);
					if (reference.supersededBy !== undefined) {
						expect(reference.status).toBe('historical');
						expect(reference.supersededBy).not.toBe(reference.scenarioId);
						expect(scenarios.has(reference.supersededBy), reference.scenarioId).toBe(true);
					}
					verifyPng(reference);
				}
			}
		},
		artifactTestTimeoutMs
	);

	it('preserves all 80 shared token values in the captured Paper source and runtime theme', () => {
		expect(manifest.tokens.source).toBe('tokens.json');
		const current = JSON.parse(readArtifact(manifest.tokens).toString('utf8')) as TokenSnapshot;
		const legacy = JSON.parse(
			readFileSync(resolve(uiRoot, 'design/paper/keeppeek-nvr-v34/tokens.json'), 'utf8')
		) as TokenSnapshot;
		expect(current.contentHash.tokens).toBe(manifest.source.tokenHash);
		expect(current.tokens).toHaveLength(manifest.source.tokenCount);
		expect(legacy.tokens).toHaveLength(80);
		expectUnique(
			current.tokens.map((token) => token.name),
			'Duplicate captured token names'
		);
		const currentTokens = new Map(current.tokens.map((token) => [token.name, token.value]));
		const css = readFileSync(resolve(uiRoot, 'src/styles/paper-theme.css'), 'utf8');
		const declarations = [...css.matchAll(/^\s*(--[a-z0-9-]+):\s*(.+);\s*$/gim)];
		expectUnique(
			declarations.map((match) => match[1]),
			'Duplicate runtime token declarations'
		);
		const runtimeTokens = new Map(declarations.map((match) => [match[1], match[2].trim()]));
		for (const token of legacy.tokens) {
			expect(currentTokens.get(token.name), token.name).toBe(token.value);
			expect(runtimeTokens.has(token.name), token.name).toBe(true);
			expect(normalizedTokenValue(runtimeTokens.get(token.name)!), token.name).toBe(
				normalizedTokenValue(token.value)
			);
		}
		const sharedNames = new Set(legacy.tokens.map((token) => token.name));
		expect(
			current.tokens
				.filter((token) => !sharedNames.has(token.name))
				.map((token) => token.name)
				.toSorted()
		).toEqual(['--color-mask', '--light-mask']);
	});
});
