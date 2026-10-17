import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { lstat, mkdir, mkdtemp, readFile, readdir, rename, rm, writeFile } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bookPages, limits, pageNames, selectedDocs, sourcePath } from './wiki-manifest';
import {
	encodePath,
	localTarget,
	parseMarkdown,
	repositoryUrl,
	rewriteMarkdown,
	wikiRawUrl,
	wikiUrl,
	type Markdown,
	type Destination
} from './wiki-markdown';

export type WikiManifest = {
	schemaVersion: 1;
	sourceCommit: string;
	pages: Record<string, string>;
	files: Record<string, string>;
};
export type BuildWikiOptions = {
	root: string;
	output: string;
	sourceCommit?: string;
	docs?: readonly string[];
};

async function noSymlinks(path: string): Promise<void> {
	const absolute = resolve(path);
	const parts = absolute.split(sep);
	let cursor = parts.shift()! || sep;
	for (const part of parts) {
		cursor = join(cursor, part);
		try {
			if ((await lstat(cursor)).isSymbolicLink())
				throw new Error(`Symlink is forbidden: ${cursor}`);
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === 'ENOENT') return;
			throw error;
		}
	}
}

async function sourceFile(root: string, path: string, maxBytes: number): Promise<Buffer> {
	const absolute = resolve(root, sourcePath(path));
	await noSymlinks(absolute);
	const stat = await lstat(absolute);
	if (!stat.isFile()) throw new Error(`Expected source file: ${path}`);
	if (stat.size > maxBytes) throw new Error(`Source size exceeds ${maxBytes} bytes: ${path}`);
	const bytes = await readFile(absolute);
	if (bytes.length > maxBytes) throw new Error(`Source size changed beyond limit: ${path}`);
	return bytes;
}

async function loadPages(root: string, docs: readonly string[]) {
	const summary = (await sourceFile(root, 'book/src/SUMMARY.md', limits.sourceBytes)).toString(
		'utf8'
	);
	const pages = pageNames([...bookPages(summary), ...docs]);
	const markdown = new Map<string, Markdown>();
	let total = 0;
	for (const path of Object.keys(pages)) {
		const bytes = await sourceFile(root, path, limits.sourceBytes);
		total += bytes.length;
		if (total > limits.totalBytes) throw new Error('Page total size exceeds limit');
		try {
			markdown.set(
				path,
				parseMarkdown(new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes))
			);
		} catch (error) {
			throw new Error(`${path}: ${(error as Error).message}`, { cause: error });
		}
	}
	return { pages, markdown, total };
}

type Context = Awaited<ReturnType<typeof loadPages>> & {
	root: string;
	sourceCommit: string;
	assets: Map<string, Buffer>;
};

async function resolveDestination(
	context: Context,
	source: string,
	destination: Destination
): Promise<string | undefined> {
	const target = localTarget(source, destination.url);
	if (!target) return;
	const absolute = resolve(context.root, target.path);
	await noSymlinks(absolute);
	const stat = await lstat(absolute);
	const page = context.pages[target.path];
	if (page) {
		if (target.fragment && !context.markdown.get(target.path)!.anchors.has(target.fragment)) {
			throw new Error(`Missing fragment: ${target.path}#${target.fragment}`);
		}
		return `${wikiUrl}/${encodeURIComponent(page)}${target.suffix}`;
	}
	const media = /\.(?:png|jpe?g|gif|svg|webp|avif|ico|mp4|webm|mov|mp3|wav|ogg|woff2?)$/i.test(
		target.path
	);
	const publicAsset = /^(?:book\/src|docs|assets)\//.test(target.path);
	if (publicAsset && (destination.asset || media)) {
		if (!context.assets.has(target.path)) {
			if (context.assets.size >= limits.assets) throw new Error('Asset count exceeds limit');
			const bytes = await sourceFile(context.root, target.path, limits.assetBytes);
			context.total += bytes.length;
			if (context.total > limits.totalBytes) throw new Error('Wiki total size exceeds limit');
			context.assets.set(target.path, bytes);
		}
		return `${wikiRawUrl}/assets/${encodePath(target.path)}${target.suffix}`;
	}
	return `${repositoryUrl}/${stat.isDirectory() ? 'tree' : 'blob'}/${context.sourceCommit}/${encodePath(target.path)}${target.suffix}`;
}

async function render(context: Context): Promise<Map<string, Buffer>> {
	const files = new Map<string, Buffer>();
	for (const [source, markdown] of context.markdown) {
		const replacements = new Map<Destination, string>();
		for (const destination of markdown.destinations) {
			try {
				const replacement = await resolveDestination(context, source, destination);
				if (replacement !== undefined) replacements.set(destination, replacement);
			} catch (error) {
				throw new Error(`${source}: ${destination.url}: ${(error as Error).message}`, {
					cause: error
				});
			}
		}
		files.set(`${context.pages[source]}.md`, Buffer.from(rewriteMarkdown(markdown, replacements)));
	}
	const navigation = Object.entries(context.pages)
		.map(([source, name]) => {
			const label = context.markdown.get(source)!.title.replace(/[[\]\\]/g, '\\$&') || name;
			return `- [${label}](${wikiUrl}/${encodeURIComponent(name)})`;
		})
		.join('\n');
	files.set(
		'Home.md',
		Buffer.from(
			`# KeepPeek documentation\n\nEdit the [repository Markdown](${repositoryUrl}/tree/${context.sourceCommit}/book/src), not this generated Wiki.\n\n## Documentation\n\n${navigation}\n`
		)
	);
	files.set(
		'_Sidebar.md',
		Buffer.from(`[KeepPeek documentation](${wikiUrl}/Home)\n\n${navigation}\n`)
	);
	for (const [path, bytes] of context.assets) files.set(`assets/${path}`, bytes);
	return files;
}

async function validateOutput(root: string, output: string): Promise<void> {
	if (!output || output.length > limits.pathBytes)
		throw new Error('An explicit output directory is required');
	const within = relative(root, output);
	if (
		within === '' ||
		(!within.startsWith(`..${sep}`) && !isAbsolute(within) && !within.startsWith(`target${sep}`))
	) {
		throw new Error('Wiki output must be outside source directories (use target/wiki)');
	}
	if (
		relative(output, root) === '' ||
		(!relative(output, root).startsWith(`..${sep}`) && !isAbsolute(relative(output, root)))
	) {
		throw new Error('Wiki output cannot contain the source repository');
	}
	await noSymlinks(root);
	await noSymlinks(output);
}

async function expose(stage: string, output: string): Promise<void> {
	const backup = `${stage}.previous`;
	let previous = false;
	try {
		const entries = await readdir(output);
		if (entries.some((entry) => entry.toLowerCase() === '.git')) {
			throw new Error('Wiki output must not be a Git checkout');
		}
		if (entries.length && !entries.includes('.wiki-manifest.json'))
			throw new Error('Refusing to replace unowned output');
		await rename(output, backup);
		previous = true;
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
	}
	try {
		await rename(stage, output);
	} catch (error) {
		if (previous) await rename(backup, output);
		throw error;
	}
	if (previous) await rm(backup, { recursive: true });
}

export async function buildWiki(options: BuildWikiOptions): Promise<WikiManifest> {
	const root = resolve(options.root);
	const output = resolve(options.output);
	await validateOutput(root, options.output ? output : '');
	const sourceCommit =
		options.sourceCommit ??
		execFileSync('git', ['rev-parse', 'HEAD'], {
			cwd: root,
			encoding: 'utf8',
			timeout: 10_000,
			maxBuffer: 1024
		}).trim();
	if (!/^[a-f0-9]{40}$/.test(sourceCommit))
		throw new Error('Source commit must be a full lowercase Git SHA');
	await mkdir(dirname(output), { recursive: true });
	await noSymlinks(output);
	const lock = `${output}.lock`;
	await mkdir(lock);
	let stage: string | undefined;
	try {
		stage = await mkdtemp(join(dirname(output), `.${basename(output)}.stage-`));
		const loaded = await loadPages(root, options.docs ?? selectedDocs);
		const files = await render({ ...loaded, root, sourceCommit, assets: new Map() });
		const manifest: WikiManifest = {
			schemaVersion: 1,
			sourceCommit,
			pages: loaded.pages,
			files: {}
		};
		for (const path of [...files.keys()].sort()) {
			const bytes = files.get(path)!;
			manifest.files[path] = createHash('sha256').update(bytes).digest('hex');
			await mkdir(dirname(join(stage, path)), { recursive: true });
			await writeFile(join(stage, path), bytes, { flag: 'wx' });
		}
		await writeFile(join(stage, '.wiki-manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`, {
			flag: 'wx'
		});
		await expose(stage, output);
		return manifest;
	} finally {
		if (stage) await rm(stage, { recursive: true, force: true });
		await rm(lock, { recursive: true });
	}
}

async function main(): Promise<void> {
	const args = process.argv.slice(2);
	const root = resolve(import.meta.dirname, '../..');
	if (args.length === 1 && args[0] === '--check') {
		await mkdir(join(root, 'target'), { recursive: true });
		const output = await mkdtemp(join(root, 'target/wiki-check-'));
		try {
			const manifest = await buildWiki({ root, output });
			console.log(
				`Wiki validated: ${Object.keys(manifest.pages).length} pages, ${Object.keys(manifest.files).length} files`
			);
		} finally {
			await rm(output, { recursive: true, force: true });
		}
	} else if (args.length === 2 && args[0] === '--output' && args[1]) {
		const manifest = await buildWiki({ root, output: resolve(args[1]) });
		console.log(
			`Wiki generated: ${Object.keys(manifest.pages).length} pages, ${Object.keys(manifest.files).length} files`
		);
	} else {
		throw new Error('Usage: bun scripts/build-wiki.ts --output DIRECTORY | --check');
	}
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
	main().catch((error: unknown) => {
		console.error(error instanceof Error ? error.message : error);
		process.exitCode = 1;
	});
}
