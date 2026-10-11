import { lstatSync, readFileSync, realpathSync, statSync } from 'node:fs';
import path from 'node:path';
import { remark } from 'remark';
import remarkGfm from 'remark-gfm';
import type { WikiPage } from './wiki-manifest';

const REPOSITORY = 'https://github.com/xnorpx/keeppeek';
export const MAX_WIKI_MARKDOWN_BYTES = 2 * 1024 * 1024;
const MAX_LINK_TARGET_BYTES = 20 * 1024 * 1024;
export const MAX_WIKI_ASSET_BYTES = 20 * 1024 * 1024;
const MAX_MARKDOWN_NODES = 200_000;

type MarkdownNode = {
	type: string;
	url?: string;
	value?: string;
	children?: MarkdownNode[];
	position?: {
		start: { offset?: number };
		end: { offset?: number };
	};
};

type DestinationRange = { start: number; end: number };
type Replacement = DestinationRange & { value: string };
type WikiAsset = { source: string; destination: string };

function canonicalRepositoryPath(repositoryRoot: string, source: string): string {
	const root = realpathSync(repositoryRoot);
	try {
		return realpathSync(path.resolve(root, ...source.split('/')));
	} catch (error) {
		if (error instanceof Error && 'code' in error && error.code === 'ENOENT') {
			throw new Error(`Missing local target: ${source}`);
		}
		throw error;
	}
}

export type ConvertedMarkdown = {
	markdown: string;
	assets: WikiAsset[];
};

export type MarkdownOptions = {
	repositoryRoot: string;
	sourceCommit: string;
	page: WikiPage;
	pages: ReadonlyMap<string, WikiPage>;
};

function isWithin(parent: string, child: string): boolean {
	const relative = path.relative(parent, child);
	return (
		relative === '' ||
		(!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative))
	);
}

function checkedRepositoryFilePath(
	repositoryRoot: string,
	source: string,
	maxBytes: number
): string {
	if (!source || source.includes('\\') || path.posix.isAbsolute(source)) {
		throw new Error(`Invalid repository source path: ${source}`);
	}
	const root = realpathSync(repositoryRoot);
	const sourcePath = canonicalRepositoryPath(root, source);
	if (!isWithin(root, sourcePath)) {
		throw new Error(`Repository source path escapes the repository: ${source}`);
	}
	const info = statSync(sourcePath);
	if (!info.isFile()) throw new Error(`Repository source is not a regular file: ${source}`);
	if (info.size > maxBytes)
		throw new Error(`Repository source exceeds ${maxBytes} bytes: ${source}`);
	return sourcePath;
}

export function assertDocumentationFile(repositoryRoot: string, source: string): void {
	const documentationRoot = source.startsWith('book/src/')
		? 'book/src'
		: source.startsWith('docs/')
			? 'docs'
			: undefined;
	if (!documentationRoot) throw new Error(`Not a documentation source file: ${source}`);
	const root = realpathSync(repositoryRoot);
	const sourceRoot = path.resolve(root, ...documentationRoot.split('/'));
	if (lstatSync(sourceRoot).isSymbolicLink()) {
		throw new Error(`Documentation source root cannot be a symbolic link: ${documentationRoot}`);
	}
	const canonicalRoot = realpathSync(sourceRoot);
	const canonicalSource = canonicalRepositoryPath(root, source);
	if (!isWithin(canonicalRoot, canonicalSource)) {
		throw new Error(`Documentation source escapes ${documentationRoot}: ${source}`);
	}
}

export function readRepositoryFile(
	repositoryRoot: string,
	source: string,
	maxBytes: number
): Buffer {
	const sourcePath = checkedRepositoryFilePath(repositoryRoot, source, maxBytes);
	const contents = readFileSync(sourcePath);
	if (contents.byteLength > maxBytes) {
		throw new Error(`Repository source exceeds ${maxBytes} bytes: ${source}`);
	}
	return contents;
}

function nodeOffsets(node: MarkdownNode): DestinationRange {
	const start = node.position?.start.offset;
	const end = node.position?.end.offset;
	if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start! > end!) {
		throw new Error(`Markdown parser did not provide source offsets for ${node.type}`);
	}
	return { start: start!, end: end! };
}

function normalizedDestination(value: string): string {
	return value.replace(/\\(.)/g, '$1').replace(/&amp;/gi, '&');
}

function readDestination(source: string, start: number): DestinationRange | undefined {
	let cursor = start;
	while (/[ \t\r\n]/.test(source[cursor] ?? '')) cursor++;
	if (source[cursor] === '<') {
		const destinationStart = ++cursor;
		while (cursor < source.length) {
			if (source[cursor] === '\\') cursor += 2;
			else if (source[cursor] === '>') return { start: destinationStart, end: cursor };
			else cursor++;
		}
		return undefined;
	}
	const destinationStart = cursor;
	let parentheses = 0;
	while (cursor < source.length) {
		const character = source[cursor];
		if (character === '\\') {
			cursor += 2;
			continue;
		}
		if (character === '(') parentheses++;
		if (character === ')') {
			if (!parentheses) break;
			parentheses--;
		}
		if (/[ \t\r\n]/.test(character)) break;
		cursor++;
	}
	return { start: destinationStart, end: cursor };
}

function inlineDestinationRange(
	markdown: string,
	range: DestinationRange,
	expectedUrl: string
): DestinationRange {
	const source = markdown.slice(range.start, range.end);
	let marker = source.indexOf('](');
	while (marker !== -1) {
		const destination = readDestination(source, marker + 2);
		if (destination) {
			const rawUrl = source.slice(destination.start, destination.end);
			if (normalizedDestination(rawUrl) === expectedUrl) {
				return {
					start: range.start + destination.start,
					end: range.start + destination.end
				};
			}
		}
		marker = source.indexOf('](', marker + 2);
	}
	throw new Error(`Could not locate Markdown destination: ${expectedUrl}`);
}

function definitionDestinationRange(
	markdown: string,
	range: DestinationRange,
	expectedUrl: string
): DestinationRange {
	const source = markdown.slice(range.start, range.end);
	const definitionEnd = source.indexOf(']:');
	if (definitionEnd < 0) throw new Error('Could not locate Markdown reference definition');
	const destination = readDestination(source, definitionEnd + 2);
	if (!destination) throw new Error(`Could not locate Markdown destination: ${expectedUrl}`);
	const rawUrl = source.slice(destination.start, destination.end);
	if (normalizedDestination(rawUrl) !== expectedUrl) {
		throw new Error(`Could not locate Markdown destination: ${expectedUrl}`);
	}
	return { start: range.start + destination.start, end: range.start + destination.end };
}

function splitSuffix(url: string): { path: string; suffix: string } {
	const suffixIndex = url.search(/[?#]/);
	return suffixIndex < 0
		? { path: url, suffix: '' }
		: { path: url.slice(0, suffixIndex), suffix: url.slice(suffixIndex) };
}

function decodePath(value: string): string {
	try {
		return decodeURIComponent(value);
	} catch {
		throw new Error(`Invalid percent-encoded Markdown path: ${value}`);
	}
}

function repositoryBlobTarget(urlValue: string): { source: string; suffix: string } | undefined {
	let url: URL;
	try {
		url = new URL(urlValue);
	} catch {
		return undefined;
	}
	if (url.origin !== 'https://github.com') return undefined;
	if (url.username || url.password) return undefined;
	const prefix = '/xnorpx/keeppeek/blob/';
	if (!url.pathname.startsWith(prefix)) return undefined;
	const parts = url.pathname.slice(prefix.length).split('/');
	if (parts.length < 2) return undefined;
	return {
		source: decodePath(parts.slice(1).join('/')),
		suffix: `${url.search}${url.hash}`
	};
}

function isExternalUrl(value: string): boolean {
	return value.startsWith('//') || /^[a-z][a-z\d+.-]*:/i.test(value);
}

function assertSupportedHtmlTargets(html: string, source: string): void {
	const attributes = /\b(?:href|src|poster)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/gi;
	for (const match of html.matchAll(attributes)) {
		const target = match[1] ?? match[2] ?? match[3] ?? '';
		if (target && !isExternalUrl(target)) {
			throw new Error(`Unsupported relative HTML link or asset in ${source}: ${target}`);
		}
	}
}

function encodedPath(value: string): string {
	return value.split('/').map(encodeURIComponent).join('/');
}

function pinnedSourceUrl(source: string, suffix: string, commit: string): string {
	return `${REPOSITORY}/blob/${commit}/${encodedPath(source)}${suffix}`;
}

function resolveLocalSource(
	repositoryRoot: string,
	currentSource: string,
	pathValue: string
): string {
	const decoded = decodePath(pathValue);
	const source = decoded.startsWith('/')
		? path.posix.normalize(decoded.slice(1))
		: path.posix.normalize(path.posix.join(path.posix.dirname(currentSource), decoded));
	if (source === '..' || source.startsWith('../') || path.posix.isAbsolute(source)) {
		throw new Error(`Local Markdown path escapes the repository: ${pathValue}`);
	}
	const root = realpathSync(repositoryRoot);
	const absolute = canonicalRepositoryPath(root, source);
	if (!isWithin(root, absolute)) {
		throw new Error(`Local Markdown path escapes the repository: ${pathValue}`);
	}
	const info = statSync(absolute);
	if (!info.isFile() && !info.isDirectory()) {
		throw new Error(`Missing local target: ${source}`);
	}
	return source;
}

function routeDestination(
	url: string,
	nodeType: 'link' | 'image',
	options: MarkdownOptions,
	assets: Map<string, WikiAsset>
): string {
	const blobTarget = repositoryBlobTarget(url);
	const localTarget = splitSuffix(url);
	const urlPath = blobTarget?.source ?? localTarget.path;
	const suffix = blobTarget?.suffix ?? localTarget.suffix;
	if (!urlPath || isExternalUrl(urlPath)) return url;
	const source = blobTarget
		? blobTarget.source
		: resolveLocalSource(options.repositoryRoot, options.page.source, urlPath);
	if (nodeType === 'image') {
		assertDocumentationFile(options.repositoryRoot, source);
		const contents = readRepositoryFile(options.repositoryRoot, source, MAX_WIKI_ASSET_BYTES);
		const destination = `assets/${source}`;
		if (!assets.has(source) && assets.size >= 100) {
			throw new Error('Wiki page exceeds the 100-asset limit');
		}
		assets.set(source, { source, destination });
		if (contents.byteLength > MAX_WIKI_ASSET_BYTES) {
			throw new Error(`Wiki image exceeds ${MAX_WIKI_ASSET_BYTES} bytes: ${source}`);
		}
		return `${encodedPath(destination)}${suffix}`;
	} else if (blobTarget) {
		checkedRepositoryFilePath(options.repositoryRoot, source, MAX_LINK_TARGET_BYTES);
	} else {
		checkedRepositoryFilePath(options.repositoryRoot, source, MAX_LINK_TARGET_BYTES);
	}
	const page = options.pages.get(source);
	if (page) return `${encodeURIComponent(page.name)}${suffix}`;
	return pinnedSourceUrl(source, suffix, options.sourceCommit);
}

function collectNodes(root: MarkdownNode): MarkdownNode[] {
	const pending = [root];
	const nodes: MarkdownNode[] = [];
	while (pending.length) {
		const node = pending.pop()!;
		nodes.push(node);
		if (nodes.length > MAX_MARKDOWN_NODES) {
			throw new Error(`Markdown exceeds the ${MAX_MARKDOWN_NODES}-node limit`);
		}
		if (node.children) pending.push(...node.children);
	}
	return nodes;
}

function replaceDestinations(markdown: string, replacements: Replacement[]): string {
	const ordered = replacements.toSorted((left, right) => right.start - left.start);
	let nextStart = markdown.length;
	for (const replacement of ordered) {
		if (replacement.end > nextStart) throw new Error('Overlapping Markdown destinations');
		markdown =
			markdown.slice(0, replacement.start) + replacement.value + markdown.slice(replacement.end);
		nextStart = replacement.start;
	}
	return markdown;
}

export function convertMarkdown(markdown: string, options: MarkdownOptions): ConvertedMarkdown {
	if (!/^[0-9a-f]{40}$/i.test(options.sourceCommit)) {
		throw new Error(`Invalid source commit: ${options.sourceCommit}`);
	}
	if (Buffer.byteLength(markdown, 'utf8') > MAX_WIKI_MARKDOWN_BYTES) {
		throw new Error(`Markdown exceeds ${MAX_WIKI_MARKDOWN_BYTES} bytes: ${options.page.source}`);
	}
	if (/\{\{#/.test(markdown)) {
		throw new Error(`Unsupported mdBook directive in ${options.page.source}`);
	}
	const tree = remark().use(remarkGfm).parse(markdown) as MarkdownNode;
	const assets = new Map<string, WikiAsset>();
	const replacements: Replacement[] = [];
	for (const node of collectNodes(tree)) {
		if (node.type === 'html') {
			assertSupportedHtmlTargets(node.value ?? '', options.page.source);
		}
		if (!node.url || !['link', 'image', 'definition'].includes(node.type)) continue;
		const offset = nodeOffsets(node);
		const routedUrl = routeDestination(
			node.url,
			node.type === 'image' ? 'image' : 'link',
			options,
			assets
		);
		if (routedUrl === node.url) continue;
		const rawNode = markdown.slice(offset.start, offset.end);
		if (
			node.type === 'link' &&
			rawNode.startsWith('<') &&
			rawNode.endsWith('>') &&
			rawNode.slice(1, -1) === node.url
		) {
			replacements.push({
				...offset,
				value: `[${node.url.replace(/([\\[\]])/g, '\\$1')}](${routedUrl})`
			});
			continue;
		}
		const destination =
			node.type === 'definition'
				? definitionDestinationRange(markdown, offset, node.url)
				: inlineDestinationRange(markdown, offset, node.url);
		replacements.push({ ...destination, value: routedUrl });
	}
	return {
		markdown: replaceDestinations(markdown, replacements),
		assets: [...assets.values()]
	};
}
