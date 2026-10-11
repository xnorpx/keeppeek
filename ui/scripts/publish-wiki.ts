import {
	copyFileSync,
	lstatSync,
	mkdirSync,
	readlinkSync,
	readdirSync,
	readFileSync,
	realpathSync,
	renameSync,
	rmSync,
	symlinkSync,
	unlinkSync,
	writeFileSync
} from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { MAX_WIKI_PAGES, wikiPageName, type WikiPage } from './wiki-manifest';
import {
	MAX_WIKI_ASSETS,
	type WikiBuildAsset,
	type WikiBuildManifest,
	type WikiBuildPage,
	wikiOwnershipMarker
} from './build-wiki';

const MANIFEST_FILE = 'Wiki-Manifest.json';
const HOME_FILE = 'Home.md';
const SIDEBAR_FILE = '_Sidebar.md';
const MAX_MANIFEST_BYTES = 2 * 1024 * 1024;
const MAX_OUTPUT_FILE_BYTES = 20 * 1024 * 1024;
const MAX_OUTPUT_BYTES = 150 * 1024 * 1024;
const MAX_WIKI_TREE_ENTRIES = 100_000;
const MAX_WIKI_TREE_BYTES = 500 * 1024 * 1024;
const HASH = /^[0-9a-f]{64}$/;
const RESERVED_PAGE_NAMES = new Set(['home', '_sidebar', 'wiki-manifest']);

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function safeRelativePath(value: string): void {
	if (
		!value ||
		value.includes('\\') ||
		path.posix.isAbsolute(value) ||
		path.posix.normalize(value) !== value ||
		value.split('/').some((part) => !part || part === '.' || part === '..') ||
		[...value].some((character) => {
			const codePoint = character.codePointAt(0)!;
			return codePoint <= 0x1f || (codePoint >= 0x7f && codePoint <= 0x9f);
		})
	) {
		throw new Error(`Invalid generated Wiki path: ${value}`);
	}
}

function pathExists(target: string): boolean {
	try {
		lstatSync(target);
		return true;
	} catch (error) {
		if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return false;
		throw error;
	}
}

function safePageName(name: string, source: string, kind: WikiPage['kind']): void {
	if (
		!name ||
		name.length > 100 ||
		name.trim() !== name ||
		/[\\/:*?"<>|]/.test(name) ||
		[...name].some((character) => {
			const codePoint = character.codePointAt(0)!;
			return codePoint <= 0x1f || (codePoint >= 0x7f && codePoint <= 0x9f);
		}) ||
		RESERVED_PAGE_NAMES.has(name.toLocaleLowerCase('en-US')) ||
		wikiPageName(source, kind) !== name
	) {
		throw new Error(`Invalid generated Wiki page name: ${name}`);
	}
}

function assertSourcePath(source: string): void {
	safeRelativePath(source);
	if (
		(!source.startsWith('book/src/') && !source.startsWith('docs/')) ||
		source === 'book/src/SUMMARY.md'
	) {
		throw new Error(`Invalid Wiki source path: ${source}`);
	}
}

function assertPage(value: unknown, names: Set<string>, sources: Set<string>): WikiBuildPage {
	if (!isRecord(value)) throw new Error('Invalid Wiki page record');
	const { name, source, title, section, kind, sha256 } = value;
	if (
		typeof name !== 'string' ||
		typeof source !== 'string' ||
		typeof title !== 'string' ||
		typeof section !== 'string' ||
		(kind !== 'book' && kind !== 'docs') ||
		typeof sha256 !== 'string' ||
		!HASH.test(sha256)
	) {
		throw new Error('Invalid Wiki page record');
	}
	assertSourcePath(source);
	if (!source.endsWith('.md')) throw new Error(`Invalid Wiki page source: ${source}`);
	if ((kind === 'book') !== source.startsWith('book/src/')) {
		throw new Error(`Wiki page source and kind do not match: ${source}`);
	}
	safePageName(name, source, kind);
	const normalizedName = name.toLocaleLowerCase('en-US');
	if (names.has(normalizedName)) throw new Error(`Duplicate generated Wiki page: ${name}`);
	if (sources.has(source)) throw new Error(`Duplicate generated Wiki source: ${source}`);
	if (!title.trim() || !section.trim() || title.length > 200 || section.length > 200) {
		throw new Error(`Invalid Wiki page title or section: ${source}`);
	}
	names.add(normalizedName);
	sources.add(source);
	return value as WikiBuildPage;
}

function assertAsset(value: unknown, sources: Set<string>): WikiBuildAsset {
	if (!isRecord(value)) throw new Error('Invalid Wiki asset record');
	const { source, destination, sha256 } = value;
	if (
		typeof source !== 'string' ||
		typeof destination !== 'string' ||
		typeof sha256 !== 'string' ||
		!HASH.test(sha256)
	) {
		throw new Error('Invalid Wiki asset record');
	}
	assertSourcePath(source);
	if (!destination.startsWith('assets/') || destination !== `assets/${source}`) {
		throw new Error(`Invalid Wiki asset destination: ${destination}`);
	}
	safeRelativePath(destination);
	if (sources.has(source)) throw new Error(`Duplicate generated Wiki asset: ${source}`);
	sources.add(source);
	return value as WikiBuildAsset;
}

function expectedOutputs(pages: WikiBuildPage[], assets: WikiBuildAsset[]): string[] {
	return [
		HOME_FILE,
		SIDEBAR_FILE,
		MANIFEST_FILE,
		...pages.map((page) => `${page.name}.md`),
		...assets.map((asset) => asset.destination)
	].sort();
}

function validateManifest(value: unknown): WikiBuildManifest {
	if (!isRecord(value)) throw new Error('Invalid Wiki build manifest');
	if (
		value.formatVersion !== 1 ||
		typeof value.sourceCommit !== 'string' ||
		!/^[0-9a-f]{40}$/i.test(value.sourceCommit) ||
		typeof value.summarySha256 !== 'string' ||
		!HASH.test(value.summarySha256) ||
		!Array.isArray(value.pages) ||
		!Array.isArray(value.assets) ||
		!Array.isArray(value.outputs)
	) {
		throw new Error('Invalid Wiki build manifest');
	}
	if (value.pages.length === 0 || value.pages.length > MAX_WIKI_PAGES) {
		throw new Error('Wiki manifest page count is outside allowed bounds');
	}
	if (value.assets.length > MAX_WIKI_ASSETS) {
		throw new Error('Wiki manifest asset count exceeds the allowed limit');
	}
	const names = new Set<string>();
	const pageSources = new Set<string>();
	const pages = value.pages.map((page) => assertPage(page, names, pageSources));
	const assetSources = new Set<string>();
	const assets = value.assets.map((asset) => assertAsset(asset, assetSources));
	const outputs = expectedOutputs(pages, assets);
	if (
		value.outputs.length !== outputs.length ||
		value.outputs.some((output, index) => output !== outputs[index])
	) {
		throw new Error('Wiki manifest output list does not match its pages and assets');
	}
	return value as unknown as WikiBuildManifest;
}

function checkedRoot(directory: string): string {
	const info = lstatSync(directory);
	if (!info.isDirectory() || info.isSymbolicLink()) {
		throw new Error(`Wiki directory must be a real directory: ${directory}`);
	}
	return realpathSync(directory);
}

function checkedFilePath(root: string, relativePath: string): string {
	safeRelativePath(relativePath);
	const segments = relativePath.split('/');
	let current = root;
	for (const [index, segment] of segments.entries()) {
		current = path.join(current, segment);
		if (!pathExists(current)) continue;
		const info = lstatSync(current);
		if (info.isSymbolicLink())
			throw new Error(`Wiki path cannot be a symbolic link: ${relativePath}`);
		if (index < segments.length - 1 && !info.isDirectory()) {
			throw new Error(`Wiki path parent is not a directory: ${relativePath}`);
		}
		if (index === segments.length - 1 && !info.isFile()) {
			throw new Error(`Wiki path is not a regular file: ${relativePath}`);
		}
	}
	return current;
}

function readWikiManifest(root: string): WikiBuildManifest {
	const manifestPath = checkedFilePath(root, MANIFEST_FILE);
	const info = lstatSync(manifestPath);
	if (info.size > MAX_MANIFEST_BYTES) throw new Error('Wiki build manifest is too large');
	let value: unknown;
	try {
		value = JSON.parse(readFileSync(manifestPath, 'utf8'));
	} catch {
		throw new Error('Unable to parse Wiki build manifest');
	}
	return validateManifest(value);
}

function readGeneratedFiles(root: string, manifest: WikiBuildManifest): Map<string, Buffer> {
	const files = new Map<string, Buffer>();
	let totalBytes = 0;
	for (const output of manifest.outputs) {
		const source = checkedFilePath(root, output);
		const info = lstatSync(source);
		if (info.size > MAX_OUTPUT_FILE_BYTES)
			throw new Error(`Generated Wiki file is too large: ${output}`);
		totalBytes += info.size;
		if (totalBytes > MAX_OUTPUT_BYTES)
			throw new Error('Generated Wiki output exceeds the size limit');
		files.set(output, readFileSync(source));
	}
	return files;
}

function checkForManualConflicts(
	wikiRoot: string,
	newOutputs: string[],
	previousOutputs: Set<string>
): void {
	for (const output of newOutputs) {
		const destination = checkedFilePath(wikiRoot, output);
		if (pathExists(destination) && !previousOutputs.has(output)) {
			throw new Error(`Generated Wiki file conflicts with an unowned page: ${output}`);
		}
	}
}

function verifyOwnedFileMarker(
	wikiRoot: string,
	output: string,
	source: string,
	sourceCommit: string
): void {
	const file = checkedFilePath(wikiRoot, output);
	if (!pathExists(file)) return;
	if (lstatSync(file).size > MAX_OUTPUT_FILE_BYTES) {
		throw new Error(`Existing Wiki file is too large: ${output}`);
	}
	const marker = Buffer.from(wikiOwnershipMarker(source, sourceCommit));
	const contents = readFileSync(file);
	if (!contents.subarray(0, marker.length).equals(marker)) {
		throw new Error(`Generated Wiki ownership marker does not match: ${output}`);
	}
}

function verifyOwnedFiles(wikiRoot: string, manifest: WikiBuildManifest): void {
	for (const page of manifest.pages) {
		verifyOwnedFileMarker(
			wikiRoot,
			`${page.name}.md`,
			page.source,
			manifest.sourceCommit
		);
	}
	verifyOwnedFileMarker(wikiRoot, HOME_FILE, HOME_FILE, manifest.sourceCommit);
	verifyOwnedFileMarker(wikiRoot, SIDEBAR_FILE, SIDEBAR_FILE, manifest.sourceCommit);
}

function copyWikiTree(source: string, destination: string): void {
	const pending = [{ source, destination }];
	let entries = 0;
	let totalBytes = 0;
	while (pending.length) {
		const current = pending.pop()!;
		mkdirSync(current.destination, { recursive: true });
		for (const entry of readdirSync(current.source, { withFileTypes: true })) {
			entries++;
			if (entries > MAX_WIKI_TREE_ENTRIES) {
				throw new Error(`Wiki checkout exceeds the ${MAX_WIKI_TREE_ENTRIES}-entry limit`);
			}
			const sourcePath = path.join(current.source, entry.name);
			const destinationPath = path.join(current.destination, entry.name);
			const info = lstatSync(sourcePath);
			if (info.isSymbolicLink()) {
				symlinkSync(readlinkSync(sourcePath), destinationPath);
			} else if (info.isDirectory()) {
				pending.push({ source: sourcePath, destination: destinationPath });
			} else if (info.isFile()) {
				totalBytes += info.size;
				if (totalBytes > MAX_WIKI_TREE_BYTES) {
					throw new Error(`Wiki checkout exceeds ${MAX_WIKI_TREE_BYTES} bytes`);
				}
				copyFileSync(sourcePath, destinationPath);
			} else {
				throw new Error(`Unsupported file in Wiki checkout: ${sourcePath}`);
			}
		}
	}
}

function applyWikiChanges(
	wikiRoot: string,
	generated: WikiBuildManifest,
	newFiles: Map<string, Buffer>
): string[] {
	const previous = pathExists(path.join(wikiRoot, MANIFEST_FILE))
		? readWikiManifest(wikiRoot)
		: undefined;
	const previousOutputs = new Set(previous?.outputs ?? []);
	const newOutputs = new Set(generated.outputs);
	if (previous) verifyOwnedFiles(wikiRoot, previous);
	checkForManualConflicts(wikiRoot, generated.outputs, previousOutputs);
	const obsoleteOutputs = [...previousOutputs].filter((output) => !newOutputs.has(output));
	for (const output of obsoleteOutputs) checkedFilePath(wikiRoot, output);
	const changed = new Set<string>();
	for (const output of obsoleteOutputs) {
		const destination = checkedFilePath(wikiRoot, output);
		if (pathExists(destination)) {
			unlinkSync(destination);
			changed.add(output);
		}
	}
	for (const [output, contents] of newFiles) {
		const destination = checkedFilePath(wikiRoot, output);
		if (pathExists(destination)) {
			if (lstatSync(destination).size > MAX_OUTPUT_FILE_BYTES) {
				throw new Error(`Existing Wiki file is too large: ${output}`);
			}
			if (readFileSync(destination).equals(contents)) continue;
		}
		mkdirSync(path.dirname(destination), { recursive: true });
		writeFileSync(destination, contents);
		changed.add(output);
	}
	return [...changed].sort();
}

function replaceWikiDirectory(stage: string, wikiRoot: string): void {
	const backup = `${wikiRoot}.backup-${randomUUID()}`;
	renameSync(wikiRoot, backup);
	try {
		renameSync(stage, wikiRoot);
	} catch (error) {
		if (!pathExists(wikiRoot)) renameSync(backup, wikiRoot);
		throw error;
	}
	rmSync(backup, { recursive: true });
}

export function syncWikiDirectory(
	wikiDirectory: string,
	generatedDirectory: string,
	expectedSourceCommit: string
): string[] {
	if (!/^[0-9a-f]{40}$/i.test(expectedSourceCommit)) {
		throw new Error(`Invalid expected source commit: ${expectedSourceCommit}`);
	}
	const wikiRoot = checkedRoot(wikiDirectory);
	const generatedRoot = checkedRoot(generatedDirectory);
	const generated = readWikiManifest(generatedRoot);
	if (generated.sourceCommit !== expectedSourceCommit) {
		throw new Error(
			`Generated Wiki source ${generated.sourceCommit} does not match ${expectedSourceCommit}`
		);
	}
	const newFiles = readGeneratedFiles(generatedRoot, generated);
	const stage = path.join(path.dirname(wikiRoot), `.${path.basename(wikiRoot)}.stage-${randomUUID()}`);
	mkdirSync(stage);
	try {
		copyWikiTree(wikiRoot, stage);
		const changed = applyWikiChanges(stage, generated, newFiles);
		if (changed.length) replaceWikiDirectory(stage, wikiRoot);
		return changed;
	} finally {
		if (pathExists(stage)) rmSync(stage, { recursive: true, force: true });
	}
}

export function assertTrustedPublicationContext(context: {
	repository: string;
	ref: string;
	event: string;
}): void {
	if (context.repository !== 'xnorpx/keeppeek') {
		throw new Error(`Wiki publication is restricted to xnorpx/keeppeek: ${context.repository}`);
	}
	if (context.ref !== 'refs/heads/main') {
		throw new Error(`Wiki publication is restricted to refs/heads/main: ${context.ref}`);
	}
	if (context.event !== 'push' && context.event !== 'workflow_dispatch') {
		throw new Error(`Wiki publication does not allow the ${context.event} event`);
	}
}

export function gitLiteralPathspec(relativePath: string): string {
	safeRelativePath(relativePath);
	return `:(literal)${relativePath}`;
}

function runCli(): void {
	const args = process.argv.slice(2);
	if (args[0] === '--verify-context') {
		assertTrustedPublicationContext({
			repository: args[1] ?? '',
			ref: args[2] ?? '',
			event: args[3] ?? ''
		});
		console.log('Wiki publication context is trusted.');
		return;
	}
	assertTrustedPublicationContext({
		repository: process.env.GITHUB_REPOSITORY ?? '',
		ref: process.env.GITHUB_REF ?? '',
		event: process.env.GITHUB_EVENT_NAME ?? ''
	});
	if (args.length !== 3) {
		throw new Error(
			'Usage: bun scripts/publish-wiki.ts <wiki-directory> <generated-directory> <source-commit>'
		);
	}
	const [wikiDirectory, generatedDirectory, sourceCommit] = args;
	const wikiRoot = checkedRoot(wikiDirectory);
	const generatedRoot = checkedRoot(generatedDirectory);
	const manifest = readWikiManifest(generatedRoot);
	const changed = syncWikiDirectory(wikiRoot, generatedRoot, sourceCommit);
	if (changed.length) {
		execFileSync('git', [
			'add',
			'--all',
			'--force',
			'--',
			...changed.map(gitLiteralPathspec)
		], { cwd: wikiRoot });
		console.log(`Staged ${changed.length} generated Wiki files from ${manifest.sourceCommit}.`);
	} else {
		console.log('Wiki output is unchanged.');
	}
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	try {
		runCli();
	} catch (error) {
		console.error(error instanceof Error ? error.message : error);
		process.exitCode = 1;
	}
}
