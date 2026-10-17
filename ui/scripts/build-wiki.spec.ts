import { afterEach, describe, expect, it } from 'vitest';
import {
	mkdir,
	mkdtemp,
	readFile,
	readdir,
	rm,
	symlink,
	truncate,
	writeFile
} from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { buildWiki } from './build-wiki';
import { bookPages, wikiName } from './wiki-manifest';
import { parseMarkdown } from './wiki-markdown';

const scratchRoot = resolve(import.meta.dirname, '../../target/wiki-tests');
const scratch: string[] = [];
const commit = 'a'.repeat(40);

async function fixture(chapter = '# Start\n\n[Next](nested/next.md#hello-world)\n') {
	await mkdir(scratchRoot, { recursive: true });
	const root = await mkdtemp(`${scratchRoot}/case-`);
	scratch.push(root);
	for (const [file, content] of Object.entries({
		'book/src/SUMMARY.md': '# Summary\n\n- [Start](start.md)\n  - [Next](nested/next.md)\n',
		'book/src/start.md': chapter,
		'book/src/nested/next.md': '# Hello *world*\n\n## Same\n\n## Same\n',
		'docs/guide.md': '# Guide\n',
		LICENSE: 'License',
		'book/src/images/a b.svg': '<svg></svg>'
	})) {
		await mkdir(dirname(resolve(root, file)), { recursive: true });
		await writeFile(resolve(root, file), content);
	}
	return root;
}

afterEach(async () => {
	await Promise.all(scratch.splice(0).map((path) => rm(path, { recursive: true, force: true })));
});

describe('Wiki manifest', () => {
	it('keeps nested SUMMARY display order and deterministic names', () => {
		expect(bookPages('- [A](a.md)\n  - [B](nested/b.md)\n')).toEqual([
			'book/src/a.md',
			'book/src/nested/b.md'
		]);
		expect(wikiName('docs/guide.md')).toBe('Engineering-guide');
		expect(wikiName('book/src/nested/café guide.md')).toBe('café-guide');
	});

	it('includes reference-style SUMMARY chapters', () => {
		expect(bookPages('- [Start][start]\n\n[start]: start.md\n')).toEqual(['book/src/start.md']);
	});

	it.each([
		'',
		'- [Missing]()',
		'- [A](../../outside.md)',
		'- [A](a.md)\n- [A](nested/A.md)',
		'- [Home](Home.md)',
		'- [Invalid](a%3Fb.md)'
	])('rejects empty, unsafe, reserved or colliding navigation: %s', (summary) => {
		expect(() => bookPages(summary)).toThrow();
	});
});

describe('Wiki generation', () => {
	it('handles HTML attributes, image references, Unicode and escaped targets without changing external bytes', async () => {
		const external = '[external](https://example.org/?a=1&amp;b=2 "Title")';
		const root = await fixture(
			`# Start\n${external}\n` +
				'<img alt="a > b src=\'missing.svg\'" data-src="missing.svg" src="images/a%20b.svg">\n' +
				'![reference][asset]\n\n[asset]: images/diagram.bin\n' +
				'[Unicode](nested/caf%C3%A9%20%28a%29.md#héllo)\n' +
				'[parentheses](nested/café%20\\(a\\).md#héllo)\n' +
				'<https://github.com/xnorpx/keeppeek/blob/main/docs/guide.md>\n' +
				'`{{#include absent}}`\n'
		);
		await writeFile(resolve(root, 'book/src/images/diagram.bin'), 'image');
		await writeFile(resolve(root, 'book/src/nested/café (a).md'), '# Héllo\n');
		const output = resolve(root, 'target/wiki');
		await buildWiki({ root, output, sourceCommit: commit, docs: ['docs/guide.md'] });
		const start = await readFile(resolve(output, 'start.md'), 'utf8');
		expect(start).toContain(external);
		expect(start).toContain('alt="a > b src=\'missing.svg\'" data-src="missing.svg"');
		expect(start).toContain(
			'[asset]: https://raw.githubusercontent.com/wiki/xnorpx/keeppeek/assets/book/src/images/diagram.bin'
		);
		expect(start).toContain(`<https://github.com/xnorpx/keeppeek/wiki/Engineering-guide>`);
		expect(start).toContain(`/blob/${commit}/book/src/nested/caf%C3%A9%20%28a%29.md#héllo`);
		expect(start).toContain('`{{#include absent}}`');
	});

	it('preserves Markdown bytes except real destinations and hashes deterministic assets', async () => {
		const unchanged = '```mermaid\nflowchart LR\nA["[literal](missing.md)"] --> B\n```\n\n';
		const root = await fixture(
			'# Start\r\n\r\n' +
				unchanged +
				'[Next **page**](nested/next.md#same-1 "Title")\n' +
				'[reference][next]\n\n[next]: <nested/next.md#hello-world> "Title"\n' +
				'![Image](<images/a b.svg>)\n<img src="images/a%20b.svg" alt="x">\n' +
				'[license](../../LICENSE)\n`[not a link](missing.md)`\n'
		);
		const output = resolve(root, 'target/wiki');
		const manifest = await buildWiki({
			root,
			output,
			sourceCommit: commit,
			docs: ['docs/guide.md']
		});
		const start = await readFile(resolve(output, 'start.md'), 'utf8');
		expect(start).toContain(unchanged);
		expect(start).toContain('# Start\r\n\r\n');
		expect(start).toContain('https://github.com/xnorpx/keeppeek/wiki/next#same-1 "Title"');
		expect(start).toContain('[next]: <https://github.com/xnorpx/keeppeek/wiki/next#hello-world>');
		expect(start).toContain(
			'https://raw.githubusercontent.com/wiki/xnorpx/keeppeek/assets/book/src/images/a%20b.svg'
		);
		expect(start).toContain(`https://github.com/xnorpx/keeppeek/blob/${commit}/LICENSE`);
		expect(start).toContain('`[not a link](missing.md)`');
		expect(manifest.schemaVersion).toBe(1);
		expect(manifest.pages['docs/guide.md']).toBe('Engineering-guide');
		expect(manifest.files['assets/book/src/images/a b.svg']).toMatch(/^[a-f0-9]{64}$/);
		expect(manifest.files['.wiki-manifest.json']).toBeUndefined();
		expect(await readFile(resolve(output, '_Sidebar.md'), 'utf8')).toContain('Engineering-guide');
		const before = await readFile(resolve(output, '.wiki-manifest.json'), 'utf8');
		await buildWiki({ root, output, sourceCommit: commit, docs: ['docs/guide.md'] });
		expect(await readFile(resolve(output, '.wiki-manifest.json'), 'utf8')).toBe(before);
	});

	it.each([
		'[missing](missing.md)',
		'[fragment](nested/next.md#absent)',
		'[escape](../../../outside.md)',
		'[encoded traversal](%2e%2e/%2e%2e/%2e%2e/outside.md)',
		'{{#include ../../LICENSE}}',
		'[scheme](javascript:alert(1))',
		'<img srcset="images/a.svg 1x">',
		'![missing](missing.png)'
	])('fails closed without changing previous output: %s', async (markdown) => {
		const root = await fixture(markdown);
		const output = resolve(root, 'target/wiki');
		await mkdir(output, { recursive: true });
		await writeFile(resolve(output, 'old.md'), 'Keep me');
		await expect(buildWiki({ root, output, sourceCommit: commit, docs: [] })).rejects.toThrow();
		expect(await readFile(resolve(output, 'old.md'), 'utf8')).toBe('Keep me');
		expect((await readdir(dirname(output))).sort()).toEqual(['wiki']);
	});

	it('rejects output in sources and symlinked inputs or output ancestors', async () => {
		const root = await fixture();
		await expect(
			buildWiki({ root, output: resolve(root, 'book/src/wiki'), sourceCommit: commit, docs: [] })
		).rejects.toThrow();
		await symlink(resolve(root, 'book/src/start.md'), resolve(root, 'book/src/link.md'));
		await writeFile(resolve(root, 'book/src/start.md'), '[symlink](link.md)');
		await expect(
			buildWiki({ root, output: resolve(root, 'target/wiki'), sourceCommit: commit, docs: [] })
		).rejects.toThrow(/symlink/i);
		await mkdir(resolve(root, 'target'), { recursive: true });
		await symlink(resolve(root, 'docs'), resolve(root, 'target/linked'));
		await expect(
			buildWiki({
				root,
				output: resolve(root, 'target/linked/wiki'),
				sourceCommit: commit,
				docs: []
			})
		).rejects.toThrow(/symlink/i);
	});

	it('rejects oversized input and invalid commit identities', async () => {
		const root = await fixture('x'.repeat(2 * 1024 * 1024 + 1));
		const output = resolve(root, 'target/wiki');
		await expect(buildWiki({ root, output, sourceCommit: commit, docs: [] })).rejects.toThrow(
			/size/i
		);
		await expect(buildWiki({ root, output, sourceCommit: 'main', docs: [] })).rejects.toThrow(
			/commit/i
		);
	});

	it('rejects oversized assets and never overwrites an unowned output directory', async () => {
		const root = await fixture('![image](images/large.png)');
		const asset = resolve(root, 'book/src/images/large.png');
		await writeFile(asset, '');
		await truncate(asset, 32 * 1024 * 1024 + 1);
		const output = resolve(root, 'target/wiki');
		await expect(buildWiki({ root, output, sourceCommit: commit, docs: [] })).rejects.toThrow(
			/size/i
		);
		await writeFile(resolve(root, 'book/src/start.md'), '# Start\n');
		await mkdir(output, { recursive: true });
		await writeFile(resolve(output, 'manual.md'), 'Do not remove');
		await expect(buildWiki({ root, output, sourceCommit: commit, docs: [] })).rejects.toThrow(
			/unowned/i
		);
		expect(await readFile(resolve(output, 'manual.md'), 'utf8')).toBe('Do not remove');
	});

	it('never replaces a Git checkout even when it contains a prior generated manifest', async () => {
		const root = await fixture();
		const output = resolve(root, 'target/wiki');
		await buildWiki({ root, output, sourceCommit: commit, docs: [] });
		await mkdir(resolve(output, '.git'));
		await writeFile(resolve(output, '.git/HEAD'), 'ref: refs/heads/main\n');
		await expect(buildWiki({ root, output, sourceCommit: commit, docs: [] })).rejects.toThrow(
			/checkout/i
		);
		expect(await readFile(resolve(output, '.git/HEAD'), 'utf8')).toBe('ref: refs/heads/main\n');
	});

	it('never copies image bytes outside approved public asset roots', async () => {
		const root = await fixture(
			'![private](../../private.png)\n[private link](../../private.png)\n'
		);
		await writeFile(resolve(root, 'private.png'), 'PRIVATE ASSET CONTENT');
		const output = resolve(root, 'target/wiki');
		const manifest = await buildWiki({ root, output, sourceCommit: commit, docs: [] });
		const start = await readFile(resolve(output, 'start.md'), 'utf8');
		expect(start).toContain(`/blob/${commit}/private.png`);
		expect(Object.keys(manifest.files).some((path) => path.includes('private.png'))).toBe(false);
	});

	it('rejects excessive AST nesting and preserves repeated heading anchors', () => {
		expect(() => parseMarkdown(`${'> '.repeat(130)}deep`)).toThrow(/bounds/i);
		expect([...parseMarkdown('# Same\n# Same\n# Same\n').anchors]).toEqual([
			'same',
			'same-1',
			'same-2'
		]);
	});

	it('covers every real SUMMARY chapter and selected engineering page', async () => {
		await mkdir(scratchRoot, { recursive: true });
		const output = await mkdtemp(`${scratchRoot}/real-`);
		scratch.push(output);
		const root = resolve(import.meta.dirname, '../..');
		const manifest = await buildWiki({ root, output, sourceCommit: commit });
		const chapters = bookPages(await readFile(resolve(root, 'book/src/SUMMARY.md'), 'utf8'));
		expect(chapters.every((path) => manifest.pages[path])).toBe(true);
		expect(Object.keys(manifest.files).length).toBeGreaterThan(chapters.length + 2);
	}, 30_000);
});
