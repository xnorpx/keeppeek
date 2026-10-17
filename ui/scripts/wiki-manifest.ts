import { posix } from 'node:path';
import { fromMarkdown } from 'mdast-util-from-markdown';

export const selectedDocs = [
	'access-control',
	'backup-and-restore',
	'camera-controls',
	'computer-vision',
	'configuration-management',
	'demo-video-publishing',
	'event-forwarder',
	'event-loop-runtime',
	'event-presentation',
	'event-search',
	'event-workflow',
	'evidence-exports',
	'external-authentication',
	'external-authentication-operations',
	'external-authentication-performance',
	'external-authentication-verification',
	'groups',
	'hikvision-isapi',
	'home-assistant',
	'macos-installation',
	'native-event-performance',
	'notifications',
	'onvif-event-verification',
	'onvif-events',
	'performance',
	'pre-recording-contract-proposal',
	'pre-recording-performance',
	'pre-recording-verification',
	'pushover',
	'recording-integrity',
	'recording-policy-audit',
	'runtime-profiles',
	'secrets',
	'state-store',
	'transcoding',
	'viewer',
	'wiki-publishing',
	'windows-service'
].map((name) => `docs/${name}.md`);

export const limits = {
	pages: 256,
	nodes: 100_000,
	depth: 128,
	sourceBytes: 2 * 1024 * 1024,
	assetBytes: 32 * 1024 * 1024,
	totalBytes: 128 * 1024 * 1024,
	assets: 1024,
	pathBytes: 4096
} as const;

export function sourcePath(path: string): string {
	if (
		!path ||
		Buffer.byteLength(path) > limits.pathBytes ||
		[...path].some(
			(character) => character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127
		) ||
		/[\\:?#]/.test(path) ||
		posix.isAbsolute(path)
	) {
		throw new Error(`Unsafe source path: ${path}`);
	}
	const normalized = posix.normalize(path);
	if (
		normalized === '..' ||
		normalized.startsWith('../') ||
		normalized.toLowerCase().split('/').includes('.git')
	) {
		throw new Error(`Source path escapes repository: ${path}`);
	}
	return normalized;
}

export function wikiName(path: string): string {
	const name = `${path.startsWith('docs/') ? 'Engineering-' : ''}${posix.basename(path, '.md').replace(/ /g, '-')}`;
	if (!/^[\p{L}\p{N}][\p{L}\p{N}_-]*$/u.test(name) || /^(home|_sidebar|_footer)$/i.test(name)) {
		throw new Error(`Invalid or reserved Wiki name: ${path}`);
	}
	return name;
}

export function pageNames(paths: readonly string[]): Record<string, string> {
	if (paths.length === 0 || paths.length > limits.pages) {
		throw new Error(`Wiki page count must be between 1 and ${limits.pages}`);
	}
	const names = new Set<string>(['home', '_sidebar', '_footer']);
	const pages: Record<string, string> = {};
	for (const path of paths) {
		const normalized = sourcePath(path);
		const name = wikiName(normalized);
		if (names.has(name.toLowerCase())) throw new Error(`Duplicate Wiki name: ${name}`);
		names.add(name.toLowerCase());
		pages[normalized] = name;
	}
	return pages;
}

export function bookPages(summary: string): string[] {
	if (Buffer.byteLength(summary) > limits.sourceBytes)
		throw new Error('SUMMARY source size exceeds limit');
	const tree = fromMarkdown(summary);
	const definitions = new Map(
		tree.children
			.filter((node) => node.type === 'definition')
			.map((node) => [node.identifier, node.url])
	);
	const stack: { node: (typeof tree.children)[number]; depth: number }[] = tree.children
		.toReversed()
		.map((node) => ({ node, depth: 0 }));
	const paths: string[] = [];
	let count = 0;
	while (stack.length) {
		const { node, depth } = stack.pop()!;
		if (++count > limits.nodes || depth > limits.depth)
			throw new Error('SUMMARY exceeds parse bounds');
		if (node.type === 'text' && /\{\{#/.test(node.value))
			throw new Error('Unsupported SUMMARY directive');
		if (node.type === 'link' || node.type === 'linkReference') {
			const url = node.type === 'link' ? node.url : definitions.get(node.identifier);
			if (!url?.endsWith('.md') || url.startsWith('/')) {
				throw new Error(`Unsupported SUMMARY target: ${url}`);
			}
			const relative = sourcePath(decodeURIComponent(url));
			paths.push(sourcePath(posix.join('book/src', relative)));
		}
		if ('children' in node) {
			for (const child of node.children.toReversed()) stack.push({ node: child, depth: depth + 1 });
		}
	}
	pageNames(paths);
	return paths;
}
