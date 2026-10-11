import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { mkdtemp, mkdir, readFile, readdir, rm, symlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { buildWiki } from './build-wiki';
import { MAX_WIKI_ASSET_BYTES } from './wiki-markdown';
import { buildWikiManifest } from './wiki-manifest';

const sourceCommit = '0123456789abcdef0123456789abcdef01234567';
let root: string;
let outputParent: string;

beforeEach(async () => {
	root = await mkdtemp(path.join(os.tmpdir(), 'keeppeek-wiki-build-'));
	outputParent = await mkdtemp(path.join(os.tmpdir(), 'keeppeek-wiki-output-'));
	await mkdir(path.join(root, 'book/src/images'), { recursive: true });
	await mkdir(path.join(root, 'docs'), { recursive: true });
	await writeFile(
		path.join(root, 'book/src/SUMMARY.md'),
		'# Getting started\n- [Start](./start.md)\n## Advanced\n- [More](./more.md)\n'
	);
	await writeFile(
		path.join(root, 'book/src/start.md'),
		'# Start\n\n[More](./more.md#details)\n![Diagram](./images/diagram.svg)\n'
	);
	await writeFile(path.join(root, 'book/src/more.md'), '# More\n\n## Details\n');
	await writeFile(path.join(root, 'book/src/images/diagram.svg'), '<svg />\n');
});

afterEach(async () => {
	await Promise.all([
		rm(root, { recursive: true, force: true }),
		rm(outputParent, { recursive: true, force: true })
	]);
});

async function snapshot(directory: string): Promise<Map<string, string>> {
	const files = new Map<string, string>();
	async function visit(current: string, prefix: string): Promise<void> {
		for (const entry of await readdir(current, { withFileTypes: true })) {
			const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
			const absolute = path.join(current, entry.name);
			if (entry.isDirectory()) await visit(absolute, relative);
			else files.set(relative, (await readFile(absolute)).toString('base64'));
		}
	}
	await visit(directory, '');
	return files;
}

describe('wiki output generation', () => {
	it('produces identical output on repeat and emits a complete navigation manifest', async () => {
		const output = path.join(outputParent, 'wiki');
		buildWiki(root, output, sourceCommit, []);
		const first = await snapshot(output);
		buildWiki(root, output, sourceCommit, []);

		expect(await snapshot(output)).toEqual(first);
		expect(await readFile(path.join(output, 'Start.md'), 'utf8')).toContain('[More](More#details)');
		expect(await readFile(path.join(output, 'assets/book/src/images/diagram.svg'), 'utf8')).toBe(
			'<svg />\n'
		);
		expect(await readFile(path.join(output, '_Sidebar.md'), 'utf8')).toContain('[More](More)');
		const manifest = JSON.parse(await readFile(path.join(output, 'Wiki-Manifest.json'), 'utf8'));
		expect(manifest.sourceCommit).toBe(sourceCommit);
		expect(manifest.outputs).toContain('Start.md');
		expect(manifest.assets).toHaveLength(1);
	});

	it('leaves the previous output untouched when source conversion fails', async () => {
		const output = path.join(outputParent, 'wiki');
		await mkdir(output);
		await writeFile(path.join(output, 'previous.txt'), 'keep this output\n');
		await writeFile(path.join(root, 'book/src/start.md'), '[Broken](./missing.md)\n');

		expect(() => buildWiki(root, output, sourceCommit, [])).toThrow(/missing local target/i);
		expect(await snapshot(output)).toEqual(
			new Map([['previous.txt', Buffer.from('keep this output\n').toString('base64')]])
		);
	});

	it('rejects an output directory that overlaps a documentation source root', () => {
		expect(() => buildWiki(root, 'book/src/generated', sourceCommit, [])).toThrow(
			/outside the documentation sources/i
		);
	});

	it('rejects an output reached through a symlink into the documentation source', async () => {
		const sourceLink = path.join(outputParent, 'source');
		await symlink(path.join(root, 'book/src'), sourceLink);

		expect(() => buildWiki(root, path.join(sourceLink, 'generated'), sourceCommit, [])).toThrow(
			/outside the documentation sources/i
		);
	});

	it('rejects a SUMMARY page whose source file is missing', async () => {
		await writeFile(
			path.join(root, 'book/src/SUMMARY.md'),
			'# Missing\n- [Missing](./missing.md)\n'
		);

		expect(() =>
			buildWiki(root, path.join(outputParent, 'missing-page'), sourceCommit, [])
		).toThrow(/missing local target/i);
	});

	it('rejects missing and oversized image assets', async () => {
		await writeFile(path.join(root, 'book/src/start.md'), '![Missing](./images/missing.svg)\n');
		expect(() => buildWiki(root, path.join(outputParent, 'missing'), sourceCommit, [])).toThrow(
			/missing local target/i
		);

		await writeFile(
			path.join(root, 'book/src/images/large.svg'),
			Buffer.alloc(MAX_WIKI_ASSET_BYTES + 1)
		);
		await writeFile(path.join(root, 'book/src/start.md'), '![Large](./images/large.svg)\n');
		expect(() => buildWiki(root, path.join(outputParent, 'large'), sourceCommit, [])).toThrow(
			/exceeds .* bytes/i
		);
	});

	it('builds the complete repository selection with its current SUMMARY ordering', async () => {
		const repositoryRoot = path.resolve(import.meta.dir, '../..');
		const summary = await readFile(path.join(repositoryRoot, 'book/src/SUMMARY.md'), 'utf8');
		const pages = buildWikiManifest(summary);
		const output = path.join(outputParent, 'complete-wiki');
		const manifest = buildWiki(repositoryRoot, output, sourceCommit);

		expect(manifest.pages.map((page) => page.source)).toEqual(pages.map((page) => page.source));
		expect(manifest.pages).toHaveLength(48);
		expect(manifest.assets).toHaveLength(6);
		expect(manifest.pages.find((page) => page.source === 'docs/macos-installation.md')?.title).toBe(
			'macOS installation'
		);
	});
});
