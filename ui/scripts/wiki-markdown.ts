import { posix } from 'node:path';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { limits, sourcePath } from './wiki-manifest';

export const wikiUrl = 'https://github.com/xnorpx/keeppeek/wiki';
export const wikiRawUrl = 'https://raw.githubusercontent.com/wiki/xnorpx/keeppeek';
export const repositoryUrl = 'https://github.com/xnorpx/keeppeek';

type Node = {
	type: string;
	value?: string;
	alt?: string | null;
	url?: string;
	identifier?: string;
	children?: Node[];
	position?: { start: { offset?: number }; end: { offset?: number } };
};

export type Destination = { start: number; end: number; url: string; asset: boolean };
export type Markdown = {
	source: string;
	destinations: Destination[];
	anchors: Set<string>;
	title: string;
};

export function encodePath(path: string): string {
	return path
		.split('/')
		.map((part) =>
			encodeURIComponent(part).replace(
				/[!'()*]/g,
				(character) => `%${character.charCodeAt(0).toString(16).toUpperCase()}`
			)
		)
		.join('/');
}

function descendants(root: Node): Node[] {
	const result: Node[] = [];
	const stack = [{ node: root, depth: 0 }];
	while (stack.length) {
		const { node, depth } = stack.pop()!;
		if (result.length >= limits.nodes || depth > limits.depth)
			throw new Error('Markdown exceeds parse bounds');
		result.push(node);
		for (const child of (node.children ?? []).toReversed())
			stack.push({ node: child, depth: depth + 1 });
	}
	return result;
}

function inlineText(node: Node): string {
	return descendants(node)
		.filter((child) => !child.children && child.type !== 'html')
		.map((child) => child.value ?? child.alt ?? '')
		.join('');
}

function destinationRange(raw: string, node: Node): [number, number] {
	if (node.type === 'link' && raw.startsWith('<')) return [1, raw.length - 1];
	let start: number;
	if (node.type === 'definition') {
		const prefix = /^\s{0,3}\[(?:\\.|[^\]\\])+\]:\s*/.exec(raw);
		if (!prefix) throw new Error('Unsupported reference definition');
		start = prefix[0].length;
	} else {
		const last = node.children?.at(-1)?.position?.end.offset;
		const labelEnd =
			last === undefined
				? closingLabel(raw)
				: raw.indexOf(']', last - node.position!.start.offset!);
		const prefix = /^\]\(\s*/.exec(raw.slice(labelEnd));
		if (!prefix) throw new Error('Unsupported inline destination');
		start = labelEnd + prefix[0].length;
	}
	if (raw[start] === '<') return [start + 1, raw.indexOf('>', start + 1)];
	let depth = 0;
	let end = start;
	for (; end < raw.length; end++) {
		if (raw[end] === '\\') end++;
		else if (raw[end] === '(') depth++;
		else if (raw[end] === ')' && depth > 0) depth--;
		else if (raw[end] === ')' || /\s/.test(raw[end])) break;
	}
	return [start, end];
}

function closingLabel(raw: string): number {
	let depth = 0;
	for (let index = raw.indexOf('['); index < raw.length; index++) {
		if (raw[index] === '\\') index++;
		else if (raw[index] === '[') depth++;
		else if (raw[index] === ']' && --depth === 0) return index;
	}
	throw new Error('Unsupported Markdown label');
}

function htmlDestinations(raw: string, offset: number): Destination[] {
	const destinations: Destination[] = [];
	// ponytail: Tokenize attributes only inside parser-identified HTML; reject multi-target assets.
	for (const tag of raw.matchAll(
		/<!--[\s\S]*?-->|<[a-z][a-z0-9:-]*(?:[^"'<>]|"[^"]*"|'[^']*')*>/gi
	)) {
		if (tag[0].startsWith('<!--')) continue;
		for (const attribute of tag[0].matchAll(
			/([^\s=/>]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+)))?/g
		)) {
			const name = attribute[1].toLowerCase();
			if (name === 'srcset') throw new Error('Unsupported HTML srcset asset');
			if (!['href', 'src', 'poster'].includes(name)) continue;
			const url = attribute[2] ?? attribute[3] ?? attribute[4];
			if (url === undefined) throw new Error(`Missing HTML ${name} target`);
			const prefix = /^[^=]+=\s*["']?/.exec(attribute[0])!;
			const start = offset + tag.index! + attribute.index! + prefix[0].length;
			destinations.push({
				start,
				end: start + url.length,
				url,
				asset: name !== 'href'
			});
		}
	}
	return destinations;
}

export function parseMarkdown(source: string): Markdown {
	if (Buffer.byteLength(source) > limits.sourceBytes)
		throw new Error('Markdown source size exceeds limit');
	const nodes = descendants(fromMarkdown(source));
	const imageReferences = new Set(
		nodes.filter((node) => node.type === 'imageReference').map((node) => node.identifier)
	);
	const destinations: Destination[] = [];
	const anchors = new Set<string>();
	const slugs = new Set<string>();
	const slugCounts = new Map<string, number>();
	let title = '';
	for (const node of nodes) {
		const start = node.position?.start.offset ?? 0;
		const end = node.position?.end.offset ?? 0;
		const raw = source.slice(start, end);
		if (node.type === 'code' || node.type === 'inlineCode') continue;
		if (node.type === 'text' && /\{\{#/.test(node.value ?? ''))
			throw new Error('Unsupported mdBook include/directive');
		if (node.type === 'heading') {
			const text = inlineText(node);
			title ||= text;
			const slug = text
				.toLowerCase()
				.replace(/[^\p{L}\p{N}\p{M}_\-\s]/gu, '')
				.replace(/\s/g, '-');
			let unique = slug;
			let index = slugCounts.get(slug) ?? 0;
			while (slugs.has(unique)) unique = `${slug}-${++index}`;
			slugCounts.set(slug, index);
			slugs.add(unique);
			anchors.add(unique);
		}
		if (node.type === 'html') {
			if (/\{\{#/.test(raw)) throw new Error('Unsupported mdBook directive');
			destinations.push(...htmlDestinations(raw, start));
			for (const id of raw.matchAll(/\b(?:id|name)=["']([^"']+)["']/g)) anchors.add(id[1]);
		}
		if (node.url !== undefined && ['link', 'image', 'definition'].includes(node.type)) {
			const [localStart, localEnd] = destinationRange(raw, node);
			if (localEnd < localStart) throw new Error('Invalid destination source range');
			destinations.push({
				start: start + localStart,
				end: start + localEnd,
				url: node.url,
				asset:
					node.type === 'image' ||
					(node.type === 'definition' && imageReferences.has(node.identifier))
			});
		}
	}
	return { source, destinations, anchors, title };
}

export function localTarget(
	source: string,
	url: string
): { path: string; suffix: string; fragment: string } | undefined {
	const repositoryTarget =
		/^https:\/\/github\.com\/xnorpx\/keeppeek\/(?:blob|tree)\/main\/(.*)$/.exec(url);
	if (repositoryTarget) return localTarget('README.md', repositoryTarget[1]);
	if (/^(https?:|mailto:|tel:|ftp:)/i.test(url) || url.startsWith('//')) return;
	if (/^[a-z][a-z0-9+.-]*:/i.test(url)) throw new Error(`Unsupported URL scheme: ${url}`);
	const separator = url.search(/[?#]/);
	const path = separator < 0 ? url : url.slice(0, separator);
	const suffix = separator < 0 ? '' : url.slice(separator);
	const decoded = decodeURIComponent(path);
	if (decoded.startsWith('/') || decoded.includes('\\'))
		throw new Error(`Unsafe local URL: ${url}`);
	return {
		path: path ? sourcePath(posix.join(posix.dirname(source), decoded)) : source,
		suffix,
		fragment: decodeURIComponent(url.includes('#') ? url.slice(url.indexOf('#') + 1) : '')
	};
}

export function rewriteMarkdown(
	markdown: Markdown,
	replacements: ReadonlyMap<Destination, string>
): string {
	const slices: string[] = [];
	let previous = markdown.source.length;
	for (const destination of markdown.destinations.toSorted((a, b) => b.start - a.start)) {
		if (destination.end > previous) throw new Error('Overlapping Markdown destinations');
		const replacement = replacements.get(destination);
		slices.push(markdown.source.slice(destination.end, previous));
		slices.push(replacement ?? markdown.source.slice(destination.start, destination.end));
		previous = destination.start;
	}
	slices.push(markdown.source.slice(0, previous));
	return slices.reverse().join('');
}
