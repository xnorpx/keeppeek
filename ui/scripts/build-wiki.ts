import { createHash, randomUUID } from 'node:crypto';
import {
	existsSync,
	lstatSync,
	mkdirSync,
	readFileSync,
	realpathSync,
	renameSync,
	rmSync,
	writeFileSync
} from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { remark } from 'remark';
import remarkGfm from 'remark-gfm';
import { assertSupportedMarkdown, buildWikiManifest, type WikiPage } from './wiki-manifest';
import {
	assertDocumentationFile,
	convertMarkdown,
	MAX_WIKI_ASSET_BYTES,
	MAX_WIKI_MARKDOWN_BYTES,
	readRepositoryFile
} from './wiki-markdown';

const MAX_TOTAL_MARKDOWN_BYTES = 40 * 1024 * 1024;
const MAX_TOTAL_ASSET_BYTES = 100 * 1024 * 1024;
export const MAX_WIKI_ASSETS = 100;
const HOME = 'Home.md';
const SIDEBAR = '_Sidebar.md';
const BUILD_MANIFEST = 'Wiki-Manifest.json';

export type WikiBuildPage = WikiPage & { sha256: string };
export type WikiBuildAsset = { source: string; destination: string; sha256: string };
export type WikiBuildManifest = {
	formatVersion: 1;
	sourceCommit: string;
	summarySha256: string;
	pages: WikiBuildPage[];
	assets: WikiBuildAsset[];
	outputs: string[];
};

type MarkdownNode = {
	type: string;
	url?: string;
	value?: string;
	depth?: number;
	children?: MarkdownNode[];
};

function sha256(contents: Uint8Array): string {
	return createHash('sha256').update(contents).digest('hex');
}

export function wikiOwnershipMarker(source: string): string {
	return `<!-- KeepPeek Wiki source ${sha256(Buffer.from(source))} -->\n\n`;
}

function isWithin(parent: string, child: string): boolean {
	const relative = path.relative(parent, child);
	return (
		relative === '' ||
		(!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative))
	);
}

function sourceRoots(repositoryRoot: string): string[] {
	return ['book/src', 'docs']
		.map((source) => path.join(repositoryRoot, source))
		.filter(existsSync)
		.map((source) => realpathSync(source));
}

function safeOutputPath(repositoryRoot: string, requestedPath: string): string {
	const root = realpathSync(repositoryRoot);
	const requested = path.resolve(root, requestedPath);
	if (existsSync(requested) && lstatSync(requested).isSymbolicLink()) {
		throw new Error(`Wiki output cannot be a symbolic link: ${requested}`);
	}
	let existingParent = requested;
	while (!existsSync(existingParent)) {
		const parent = path.dirname(existingParent);
		if (parent === existingParent) throw new Error(`Unable to resolve Wiki output: ${requested}`);
		existingParent = parent;
	}
	const output = path.resolve(
		realpathSync(existingParent),
		path.relative(existingParent, requested)
	);
	for (const sourceRoot of sourceRoots(root)) {
		if (isWithin(output, sourceRoot) || isWithin(sourceRoot, output)) {
			throw new Error(`Wiki output must be outside the documentation sources: ${output}`);
		}
	}
	return output;
}

function groupPages(pages: WikiPage[]): Map<string, WikiPage[]> {
	const groups = new Map<string, WikiPage[]>();
	for (const page of pages) {
		const group = groups.get(page.section) ?? [];
		group.push(page);
		groups.set(page.section, group);
	}
	return groups;
}

function markdownTitle(markdown: string, fallback: string): string {
	const tree = remark().use(remarkGfm).parse(markdown) as MarkdownNode;
	const heading = tree.children?.find((node) => node.type === 'heading' && node.depth === 1);
	if (!heading?.children) return fallback;
	const pending = heading.children.toReversed();
	const parts: string[] = [];
	while (pending.length) {
		const node = pending.pop()!;
		if (node.type === 'break') parts.push(' ');
		else if (node.value) parts.push(node.value);
		if (node.children) pending.push(...node.children.toReversed());
	}
	return parts.join('').trim() || fallback;
}

function wikiHref(name: string): string {
	return encodeURIComponent(name);
}

function navigationMarkdown(
	pages: WikiPage[],
	sourceCommit: string
): {
	home: string;
	sidebar: string;
} {
	const groups = [...groupPages(pages)];
	const home = [
		'# KeepPeek Documentation',
		'',
		'This Wiki is generated from Markdown in the KeepPeek repository. Edit `book/src/` and the selected `docs/` pages in a reviewed pull request; do not edit generated Wiki pages directly.',
		'',
		...(pages.some((page) => page.source === 'docs/wiki-publishing.md')
			? [
					'See [Wiki publishing](Engineering-Wiki-publishing) for the page mapping, local preview, and recovery steps.'
				]
			: []),
		'GitHub Pages remains an automatically generated mdBook mirror.',
		'',
		`[KeepPeek source repository](${`https://github.com/xnorpx/keeppeek/tree/${sourceCommit}`})`,
		'',
		...groups.flatMap(([section, entries]) => [
			`## ${section}`,
			'',
			...entries.map((page) => `- [${page.title}](${wikiHref(page.name)})`),
			''
		])
	].join('\n');
	const sidebar = [
		'[Home](Home)',
		'',
		...groups.flatMap(([section, entries]) => [
			`## ${section}`,
			...entries.map((page) => `- [${page.title}](${wikiHref(page.name)})`),
			''
		])
	].join('\n');
	return { home, sidebar };
}

function localPath(url: string): string {
	const suffixIndex = url.search(/[?#]/);
	const target = suffixIndex < 0 ? url : url.slice(0, suffixIndex);
	try {
		return decodeURIComponent(target);
	} catch {
		throw new Error(`Invalid generated Wiki link: ${url}`);
	}
}

function assertOutputLink(
	url: string,
	page: string,
	outputRoot: string,
	pagePaths: Set<string>,
	assetPaths: Set<string>
): void {
	if (!url || url.startsWith('//') || /^[a-z][a-z\d+.-]*:/i.test(url)) return;
	const target = localPath(url);
	if (!target) return;
	if (
		target.includes('\\') ||
		target.startsWith('/') ||
		target.split('/').some((segment) => segment === '..' || segment === '.')
	) {
		throw new Error(`Invalid generated Wiki path in ${page}: ${url}`);
	}
	const pageTarget = `${target}.md`;
	if (pagePaths.has(pageTarget)) return;
	if (assetPaths.has(target) && existsSync(path.join(outputRoot, target))) return;
	throw new Error(`Unresolved generated Wiki link in ${page}: ${url}`);
}

function verifyLinkGraph(outputRoot: string, pageFiles: string[], assetPaths: Set<string>): void {
	const pagePaths = new Set(pageFiles);
	for (const page of pageFiles) {
		const markdown = readFileSync(path.join(outputRoot, page), 'utf8');
		const tree = remark().use(remarkGfm).parse(markdown) as MarkdownNode;
		const pending = [tree];
		while (pending.length) {
			const node = pending.pop()!;
			if (node.url) assertOutputLink(node.url, page, outputRoot, pagePaths, assetPaths);
			if (node.children) pending.push(...node.children);
		}
	}
}

function writeFile(outputRoot: string, relativePath: string, contents: Uint8Array): void {
	const destination = path.join(outputRoot, ...relativePath.split('/'));
	mkdirSync(path.dirname(destination), { recursive: true });
	writeFileSync(destination, contents);
}

function exposeOutput(stage: string, output: string): void {
	const backup = `${output}.backup-${randomUUID()}`;
	let movedOldOutput = false;
	if (existsSync(output)) {
		if (!lstatSync(output).isDirectory())
			throw new Error(`Wiki output is not a directory: ${output}`);
		renameSync(output, backup);
		movedOldOutput = true;
	}
	try {
		renameSync(stage, output);
	} catch (error) {
		if (movedOldOutput && !existsSync(output)) renameSync(backup, output);
		throw error;
	}
	if (movedOldOutput) rmSync(backup, { recursive: true });
}

function renderSourcePages(
	root: string,
	stage: string,
	summaryBytes: Buffer,
	pages: WikiPage[],
	sourceCommit: string,
	pageBySource: Map<string, WikiPage>
): { pages: WikiBuildPage[]; assets: Map<string, WikiBuildAsset> } {
	const pageRecords: WikiBuildPage[] = [];
	const assetRecords = new Map<string, WikiBuildAsset>();
	let totalMarkdownBytes = summaryBytes.byteLength;
	let totalAssetBytes = 0;
	for (const page of pages) {
		assertDocumentationFile(root, page.source);
		const source = readRepositoryFile(root, page.source, MAX_WIKI_MARKDOWN_BYTES);
		totalMarkdownBytes += source.byteLength;
		if (totalMarkdownBytes > MAX_TOTAL_MARKDOWN_BYTES) {
			throw new Error(`Wiki Markdown exceeds ${MAX_TOTAL_MARKDOWN_BYTES} total bytes`);
		}
		const markdown = new TextDecoder('utf-8', { fatal: true }).decode(source);
		assertSupportedMarkdown(markdown, page.source);
		if (page.kind === 'docs') page.title = markdownTitle(markdown, page.title);
		const converted = convertMarkdown(markdown, {
			repositoryRoot: root,
			sourceCommit,
			page,
			pages: pageBySource
		});
		writeFile(
			stage,
			`${page.name}.md`,
			Buffer.from(`${wikiOwnershipMarker(page.source)}${converted.markdown}`, 'utf8')
		);
		pageRecords.push({ ...page, sha256: sha256(source) });
		for (const asset of converted.assets) {
			if (assetRecords.has(asset.source)) continue;
			if (assetRecords.size >= MAX_WIKI_ASSETS) {
				throw new Error(`Wiki exceeds the ${MAX_WIKI_ASSETS}-asset limit`);
			}
			assertDocumentationFile(root, asset.source);
			const contents = readRepositoryFile(root, asset.source, MAX_WIKI_ASSET_BYTES);
			totalAssetBytes += contents.byteLength;
			if (totalAssetBytes > MAX_TOTAL_ASSET_BYTES) {
				throw new Error(`Wiki assets exceed ${MAX_TOTAL_ASSET_BYTES} total bytes`);
			}
			writeFile(stage, asset.destination, contents);
			assetRecords.set(asset.source, { ...asset, sha256: sha256(contents) });
		}
	}
	return { pages: pageRecords, assets: assetRecords };
}

function comparePaths(left: string, right: string): number {
	if (left < right) return -1;
	if (left > right) return 1;
	return 0;
}

function writeBuildManifest(
	stage: string,
	sourceCommit: string,
	summaryBytes: Buffer,
	pages: WikiBuildPage[],
	assetRecords: Map<string, WikiBuildAsset>
): WikiBuildManifest {
	const navigation = navigationMarkdown(pages, sourceCommit);
	writeFile(stage, HOME, Buffer.from(`${wikiOwnershipMarker(HOME)}${navigation.home}`));
	writeFile(stage, SIDEBAR, Buffer.from(`${wikiOwnershipMarker(SIDEBAR)}${navigation.sidebar}`));
	const assets = [...assetRecords.values()].sort((left, right) =>
		comparePaths(left.destination, right.destination)
	);
	const pageFiles = [HOME, SIDEBAR, ...pages.map((page) => `${page.name}.md`)];
	const outputs = [...pageFiles, ...assets.map((asset) => asset.destination), BUILD_MANIFEST].sort(
		comparePaths
	);
	verifyLinkGraph(stage, pageFiles, new Set(assets.map((asset) => asset.destination)));
	const manifest: WikiBuildManifest = {
		formatVersion: 1,
		sourceCommit,
		summarySha256: sha256(summaryBytes),
		pages,
		assets,
		outputs
	};
	writeFile(stage, BUILD_MANIFEST, Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`));
	return manifest;
}

export function buildWiki(
	repositoryRoot: string,
	outputDirectory: string,
	sourceCommit: string,
	docsSources?: readonly string[]
): WikiBuildManifest {
	if (!/^[0-9a-f]{40}$/i.test(sourceCommit)) {
		throw new Error(`Invalid source commit: ${sourceCommit}`);
	}
	const root = realpathSync(repositoryRoot);
	const output = safeOutputPath(root, outputDirectory);
	const summaryBytes = readRepositoryFile(root, 'book/src/SUMMARY.md', MAX_WIKI_MARKDOWN_BYTES);
	const pages = buildWikiManifest(summaryBytes.toString('utf8'), docsSources);
	const pageBySource = new Map(pages.map((page) => [page.source, page]));
	const parent = path.dirname(output);
	mkdirSync(parent, { recursive: true });
	const stage = path.join(parent, `.${path.basename(output)}.stage-${randomUUID()}`);
	mkdirSync(stage);
	try {
		const rendered = renderSourcePages(
			root,
			stage,
			summaryBytes,
			pages,
			sourceCommit,
			pageBySource
		);
		const manifest = writeBuildManifest(
			stage,
			sourceCommit,
			summaryBytes,
			rendered.pages,
			rendered.assets
		);
		exposeOutput(stage, output);
		return manifest;
	} finally {
		if (existsSync(stage)) rmSync(stage, { recursive: true, force: true });
	}
}

function runCli(): void {
	const outputDirectory = process.argv[2];
	if (!outputDirectory) throw new Error('Usage: bun scripts/build-wiki.ts <output-directory>');
	const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
	const sourceCommit = execFileSync('git', ['rev-parse', '--verify', 'HEAD'], {
		cwd: repositoryRoot,
		encoding: 'utf8'
	}).trim();
	const manifest = buildWiki(repositoryRoot, outputDirectory, sourceCommit);
	console.log(
		`Generated ${manifest.pages.length} Wiki pages and ${manifest.assets.length} assets at ${path.resolve(repositoryRoot, outputDirectory)}`
	);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
	try {
		runCli();
	} catch (error) {
		console.error(error instanceof Error ? error.message : error);
		process.exitCode = 1;
	}
}
