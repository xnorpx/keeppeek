import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
	lstatSync,
	mkdirSync,
	opendirSync,
	readFileSync,
	realpathSync,
	unlinkSync,
	writeFileSync
} from 'node:fs';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';

const manifestName = '.wiki-manifest.json';
const filesMax = 1024;
const fileBytesMax = 32 * 1024 * 1024;
const totalBytesMax = 128 * 1024 * 1024;
const manifestBytesMax = 1024 * 1024;

type Manifest = {
	schemaVersion: 1;
	sourceCommit: string;
	pages: Record<string, string>;
	files: Record<string, string>;
};

/** Validates both trees before updating only manifest-owned wiki files. Does not push. */
export function publishWiki(
	generatedDirectory: string,
	wikiDirectory: string,
	expectedSourceCommit?: string
): boolean {
	const generated = realpathSync(generatedDirectory);
	const wiki = realpathSync(wikiDirectory);
	if (contains(generated, wiki) || contains(wiki, generated)) {
		throw new Error('Generated output and wiki checkout must not overlap.');
	}
	if (lstatSync(generatedDirectory).isSymbolicLink() || lstatSync(wikiDirectory).isSymbolicLink()) {
		throw new Error('Root directories must not be symlinks.');
	}
	assertCheckout(wiki);
	const manifestBytes = readBounded(join(generated, manifestName), manifestBytesMax);
	const incoming = readManifest(manifestBytes);
	if (expectedSourceCommit !== undefined && incoming.sourceCommit !== expectedSourceCommit) {
		throw new Error('Wiki manifest source commit does not match the trusted GITHUB_SHA.');
	}
	const content = readOwnedFiles(generated, incoming, true);
	const previousPath = safeTarget(wiki, manifestName);
	const previous = previousPath
		? readManifest(readBounded(previousPath, manifestBytesMax))
		: undefined;
	if (previous) readOwnedFiles(wiki, previous, false);
	for (const [path, bytes] of content) {
		const existing = safeTarget(wiki, path);
		if (existing && !Object.hasOwn(previous?.files ?? {}, path)) {
			if (!readBounded(existing, fileBytesMax).equals(bytes)) {
				throw new Error(`Manual file collision: ${path}. Back up and remove it deliberately.`);
			}
		}
	}
	const owned = [
		...new Set([...Object.keys(previous?.files ?? {}), ...content.keys(), manifestName])
	];
	assertNoCaseCollisions(wiki, owned);
	// ponytail: One local writer owns this checkout. Use an isolated clone for concurrent publishers.
	for (const path of Object.keys(previous?.files ?? {})) {
		if (!content.has(path)) unlinkSync(join(wiki, path));
	}
	for (const [path, bytes] of content) {
		mkdirSync(dirname(join(wiki, path)), { recursive: true });
		writeFileSync(join(wiki, path), bytes);
	}
	writeFileSync(join(wiki, manifestName), manifestBytes);
	git(wiki, ['add', '--force', '--', ...owned]);
	if (!git(wiki, ['diff', '--cached', '--name-only', '-z'])) return false;
	git(wiki, [
		'commit',
		'--only',
		'-m',
		`Publish wiki from ${incoming.sourceCommit}\n\nCo-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`,
		'--',
		...owned
	]);
	return true;
}

function contains(parent: string, child: string): boolean {
	const path = relative(parent, child);
	return path === '' || (!isAbsolute(path) && path !== '..' && !path.startsWith(`..${sep}`));
}

function git(directory: string, args: string[]): string {
	return execFileSync(
		'git',
		[
			'-C',
			directory,
			'-c',
			'core.hooksPath=/dev/null',
			'-c',
			'core.fsmonitor=false',
			'-c',
			'commit.gpgSign=false',
			...args
		],
		{
			encoding: 'utf8',
			timeout: 30_000,
			killSignal: 'SIGKILL',
			maxBuffer: 4 * 1024 * 1024,
			env: {
				...process.env,
				GIT_LITERAL_PATHSPECS: '1',
				GIT_TERMINAL_PROMPT: '0',
				GIT_OPTIONAL_LOCKS: '0'
			},
			stdio: ['ignore', 'pipe', 'pipe']
		}
	);
}

function assertCheckout(wiki: string): void {
	let metadata;
	try {
		metadata = lstatSync(join(wiki, '.git'));
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === 'ENOENT') {
			throw new Error('Wiki path must be the checkout root with its own .git directory.');
		}
		throw error;
	}
	if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
		throw new Error('Wiki must be a regular clone with its own .git directory.');
	}
	if (realpathSync(git(wiki, ['rev-parse', '--show-toplevel']).trim()) !== wiki) {
		throw new Error('Wiki path must be the checkout root.');
	}
	git(wiki, ['symbolic-ref', '--quiet', 'HEAD']);
	if (git(wiki, ['status', '--porcelain=v1', '--untracked-files=all'])) {
		throw new Error('Wiki checkout must be clean, including untracked files.');
	}
}

function record(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function validatePath(path: string): void {
	if (
		path.length === 0 ||
		Buffer.byteLength(path) > 4096 ||
		/[\\:]/.test(path) ||
		[...path].some(
			(character) => character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127
		) ||
		path.split('/').length > 16 ||
		path.split('/').some((part) => {
			return (
				!part ||
				part === '.' ||
				part === '..' ||
				/[. ]$/.test(part) ||
				part.toLowerCase().startsWith('.git') ||
				part.toLowerCase() === manifestName
			);
		})
	) {
		throw new Error(`Unsafe manifest path: ${JSON.stringify(path)}`);
	}
}

function readManifest(bytes: Buffer): Manifest {
	const value: unknown = JSON.parse(bytes.toString('utf8'));
	if (
		!record(value) ||
		value.schemaVersion !== 1 ||
		typeof value.sourceCommit !== 'string' ||
		!/^(?:[a-f0-9]{40}|[a-f0-9]{64})$/.test(value.sourceCommit) ||
		!record(value.pages) ||
		!record(value.files)
	) {
		throw new Error('Invalid wiki manifest schema, version, or source commit.');
	}
	const paths = Object.keys(value.files);
	if (paths.length === 0 || paths.length > filesMax || Object.keys(value.pages).length > filesMax) {
		throw new Error('Wiki manifest exceeds the file/page count limits or has no files.');
	}
	const names = new Set<string>();
	for (const [path, hash] of Object.entries(value.files)) {
		validatePath(path);
		if (typeof hash !== 'string' || !/^[a-f0-9]{64}$/.test(hash)) {
			throw new Error(`Invalid SHA-256 hash for ${path}.`);
		}
		const folded = path.toLowerCase();
		if (names.has(folded)) throw new Error(`Duplicate case-insensitive path: ${path}`);
		names.add(folded);
	}
	for (const path of paths) {
		const parts = path.toLowerCase().split('/');
		for (let index = 1; index < parts.length; index++) {
			if (names.has(parts.slice(0, index).join('/'))) {
				throw new Error(`Conflicting file/directory path: ${path}`);
			}
		}
	}
	for (const [source, page] of Object.entries(value.pages)) {
		validatePath(source);
		if (typeof page !== 'string') throw new Error('Invalid wiki page name.');
		validatePath(page);
		if (page.includes('/') || !Object.hasOwn(value.files, `${page}.md`)) {
			throw new Error(`Invalid wiki page output path: ${page}`);
		}
	}
	return value as Manifest;
}

function readBounded(path: string, bytesMax: number): Buffer {
	const info = lstatSync(path);
	if (!info.isFile() || info.isSymbolicLink()) {
		throw new Error(`Expected a regular file, not a symlink: ${path}`);
	}
	if (info.size > bytesMax) throw new Error(`File size limit exceeded: ${path}`);
	return readFileSync(path);
}

function safeTarget(directory: string, path: string): string | undefined {
	const parts = path.split('/');
	let target = directory;
	for (let index = 0; index < parts.length; index++) {
		target = join(target, parts[index]);
		let info;
		try {
			info = lstatSync(target);
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
			throw error;
		}
		if (info.isSymbolicLink()) throw new Error(`Symlink in wiki path: ${path}`);
		if (index < parts.length - 1 && !info.isDirectory()) {
			throw new Error(`Expected a directory in wiki path: ${path}`);
		}
		if (index === parts.length - 1 && !info.isFile()) {
			throw new Error(`Expected a regular wiki file: ${path}`);
		}
	}
	return target;
}

function assertNoCaseCollisions(wiki: string, paths: string[]): void {
	const directories = new Map<string, Map<string, string>>();
	for (const path of paths) {
		const parts = path.split('/');
		for (let index = 0; index < parts.length; index++) {
			const parent = join(wiki, ...parts.slice(0, index));
			const names = directories.get(parent) ?? new Map<string, string>();
			const folded = parts[index].toLowerCase();
			const previous = names.get(folded);
			if (previous !== undefined && previous !== parts[index]) {
				throw new Error(`Case-insensitive manifest path collision: ${path}`);
			}
			names.set(folded, parts[index]);
			directories.set(parent, names);
		}
	}
	let entries = 0;
	for (const [directory, names] of directories) {
		let handle;
		try {
			handle = opendirSync(directory);
		} catch (error) {
			if ((error as NodeJS.ErrnoException).code === 'ENOENT') continue;
			throw error;
		}
		try {
			for (let entry = handle.readSync(); entry; entry = handle.readSync()) {
				if (++entries > filesMax * 34 + 1) throw new Error('Wiki directory entry limit exceeded.');
				const expected = names.get(entry.name.toLowerCase());
				if (expected !== undefined && expected !== entry.name) {
					throw new Error(`Case-insensitive manual file collision: ${join(directory, entry.name)}`);
				}
			}
		} finally {
			handle.closeSync();
		}
	}
}

function readOwnedFiles(
	directory: string,
	manifest: Manifest,
	exact: boolean
): Map<string, Buffer> {
	if (exact) assertGeneratedTree(directory, manifest);
	const files = new Map<string, Buffer>();
	let totalBytes = 0;
	for (const [path, hash] of Object.entries(manifest.files)) {
		const target = safeTarget(directory, path);
		if (!target) throw new Error(`Missing owned file: ${path}`);
		const bytes = readBounded(target, fileBytesMax);
		totalBytes += bytes.length;
		if (totalBytes > totalBytesMax) throw new Error('Wiki total file size limit exceeded.');
		if (createHash('sha256').update(bytes).digest('hex') !== hash) {
			throw new Error(`Owned file was modified or has an invalid hash: ${path}`);
		}
		files.set(path, bytes);
	}
	return files;
}

function assertGeneratedTree(directory: string, manifest: Manifest): void {
	const pending = [''];
	const paths = Object.keys(manifest.files);
	let entries = 0;
	while (pending.length > 0) {
		const prefix = pending.pop()!;
		const handle = opendirSync(join(directory, prefix));
		try {
			for (let entry = handle.readSync(); entry; entry = handle.readSync()) {
				if (++entries > filesMax * 17 + 1) throw new Error('Generated tree entry limit exceeded.');
				const path = prefix ? `${prefix}/${entry.name}` : entry.name;
				if (path === manifestName && entry.isFile()) continue;
				validatePath(path);
				if (entry.isDirectory() && paths.some((owned) => owned.startsWith(`${path}/`))) {
					pending.push(path);
				} else if (!entry.isFile() || !Object.hasOwn(manifest.files, path)) {
					throw new Error(`Unowned extra entry or symlink in generated tree: ${path}`);
				}
			}
		} finally {
			handle.closeSync();
		}
	}
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
	try {
		if (process.argv.length !== 4) {
			throw new Error(
				'Usage: bun ui/scripts/publish-wiki.ts <generated directory> <wiki checkout>'
			);
		}
		console.log(
			publishWiki(process.argv[2], process.argv[3], process.env.GITHUB_SHA)
				? 'Wiki commit created.'
				: 'Wiki unchanged.'
		);
	} catch (error) {
		console.error(error instanceof Error ? error.message : 'Wiki publication failed.');
		process.exitCode = 1;
	}
}
