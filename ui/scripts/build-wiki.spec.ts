import { afterEach, describe, expect, it } from 'vitest';
import { execFileSync } from 'node:child_process';
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
import { buildWiki, validateWikiFiles } from './build-wiki';
import { bookPages, limits, wikiName } from './wiki-manifest';
import { localTarget, parseMarkdown } from './wiki-markdown';
import { publishWiki } from './publish-wiki';

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
	it('resolves direct repository and own-source links without changing external URLs', () => {
		expect(localTarget('book/src/start.md', '#start')).toEqual({
			path: 'book/src/start.md',
			suffix: '#start',
			fragment: 'start'
		});
		expect(
			localTarget(
				'book/src/start.md',
				'https://github.com/xnorpx/keeppeek/blob/main/README.md#keeppeek'
			)
		).toEqual({ path: 'README.md', suffix: '#keeppeek', fragment: 'keeppeek' });
		expect(
			localTarget('book/src/start.md', 'https://example.org/README.md#missing')
		).toBeUndefined();
	});

	it.each([2, 10_000])('does not recursively unwrap %i nested repository prefixes', (count) => {
		const prefix = 'https://github.com/xnorpx/keeppeek/blob/main/';
		expect(localTarget('book/src/start.md', `${prefix.repeat(count)}README.md`)).toBeUndefined();
	});

	it('checks unselected Markdown fragments without copying their source', async () => {
		const root = await fixture('[Readme](../../README.md#keeppeek)\n[Again](../../README.md#more)');
		await writeFile(resolve(root, 'README.md'), '# KeepPeek\n\n## More\n');
		const output = resolve(root, 'target/wiki');
		const manifest = await buildWiki({ root, output, sourceCommit: commit, docs: [] });
		expect(await readFile(resolve(output, 'start.md'), 'utf8')).toContain(
			`/blob/${commit}/README.md#keeppeek`
		);
		expect(manifest.pages['README.md']).toBeUndefined();
		expect(manifest.files['README.md']).toBeUndefined();
		await writeFile(resolve(root, 'book/src/start.md'), '[Readme](../../README.md#nonexistent)');
		await expect(buildWiki({ root, output, sourceCommit: commit, docs: [] })).rejects.toThrow(
			'Missing fragment: README.md#nonexistent'
		);
	});

	it('bounds the number of cached unselected Markdown anchor sets', async () => {
		const root = await fixture();
		const links: string[] = [];
		for (let index = 0; index <= limits.pages; index++) {
			const path = `extra-${index}.md`;
			await writeFile(resolve(root, path), '# Extra\n');
			links.push(`[Extra](../../${path}#extra)`);
		}
		await writeFile(resolve(root, 'book/src/start.md'), links.join('\n'));
		await expect(
			buildWiki({ root, output: resolve(root, 'target/wiki'), sourceCommit: commit, docs: [] })
		).rejects.toThrow(/anchor.*count.*limit/i);
	});

	it.each([false, true])(
		'bounds cumulative fragment source reads and reuses cached anchors (repeated=%s)',
		async (repeated) => {
			const root = await fixture();
			const markdown = `# Extra\n\n<!--${'x'.repeat(limits.sourceBytes - 32)}-->\n`;
			const links: string[] = [];
			const count = Math.ceil(limits.totalBytes / Buffer.byteLength(markdown)) + 1;
			for (let index = 0; index < count; index++) {
				const path = repeated ? 'extra.md' : `extra-${index}.md`;
				if (!repeated || index === 0) await writeFile(resolve(root, path), markdown);
				links.push(`[Extra](../../${path}#extra)`);
			}
			await writeFile(resolve(root, 'book/src/start.md'), links.join('\n'));
			const build = buildWiki({
				root,
				output: resolve(root, 'target/wiki'),
				sourceCommit: commit,
				docs: []
			});
			if (repeated) await expect(build).resolves.toMatchObject({ schemaVersion: 1 });
			else await expect(build).rejects.toThrow(/size/i);
		},
		30_000
	);

	it('rejects oversized unselected fragment targets before parsing them', async () => {
		const root = await fixture('[Readme](../../README.md#keeppeek)');
		await writeFile(resolve(root, 'README.md'), '# KeepPeek\n');
		await truncate(resolve(root, 'README.md'), limits.sourceBytes + 1);
		await expect(
			buildWiki({ root, output: resolve(root, 'target/wiki'), sourceCommit: commit, docs: [] })
		).rejects.toThrow(/size/i);
	});

	it('matches publisher limits for file count, per-file bytes and total output bytes', () => {
		expect(() =>
			validateWikiFiles(
				new Map(Array.from({ length: 1025 }, (_, index) => [`${index}.md`, Buffer.alloc(0)]))
			)
		).toThrow(/count/i);
		const bytes = Buffer.alloc(16 * 1024 * 1024);
		const files = new Map(Array.from({ length: 4 }, (_, index) => [`${index}.md`, bytes]));
		expect(() => validateWikiFiles(files)).not.toThrow();
		files.set('extra.md', Buffer.alloc(1));
		expect(() => validateWikiFiles(files)).toThrow(/total/i);
		expect(() => validateWikiFiles(new Map([['huge.md', Buffer.alloc(bytes.length + 1)]]))).toThrow(
			/size/i
		);
	});

	it('rejects publisher-incompatible output paths and case-insensitive asset collisions', () => {
		for (const path of [
			'assets/.github/image.png',
			'assets/trailing./image.png',
			`${'a/'.repeat(16)}image.png`
		]) {
			expect(() => validateWikiFiles(new Map([[path, Buffer.alloc(0)]]))).toThrow(/path/i);
		}
		expect(() =>
			validateWikiFiles(
				new Map([
					['assets/a.png', Buffer.alloc(0)],
					['assets/A.png', Buffer.alloc(0)]
				])
			)
		).toThrow(/collision/i);
	});

	it('handles HTML attributes, image references, Unicode and escaped targets without changing external bytes', async () => {
		const external = '[external](https://example.org/?a=1&amp;b=2 "Title")';
		const root = await fixture(
			`# Start\n${external}\n` +
				'<img alt="a > b src=\'missing.svg\'" data-src="missing.svg" src="images/a%20b.svg">\n' +
				'![reference][asset]\n\n[asset]: images/diagram\n' +
				'[Unicode](nested/caf%C3%A9%20%28a%29.md#héllo)\n' +
				'[selected Unicode](nested/caf%C3%A9%20guide.md#h%C3%A9llo)\n' +
				'[parentheses](nested/café%20\\(a\\).md#héllo)\n' +
				'<https://github.com/xnorpx/keeppeek/blob/main/docs/guide.md>\n' +
				'`{{#include absent}}`\n'
		);
		await writeFile(resolve(root, 'book/src/images/diagram'), 'image');
		await writeFile(resolve(root, 'book/src/nested/café (a).md'), '# Héllo\n');
		await writeFile(resolve(root, 'book/src/nested/café guide.md'), '# Héllo\n');
		const summary = resolve(root, 'book/src/SUMMARY.md');
		await writeFile(
			summary,
			`${await readFile(summary, 'utf8')}\n- [Unicode](nested/caf%C3%A9%20guide.md)\n`
		);
		const output = resolve(root, 'target/wiki');
		await buildWiki({ root, output, sourceCommit: commit, docs: ['docs/guide.md'] });
		const start = await readFile(resolve(output, 'start.md'), 'utf8');
		expect(start).toContain(external);
		expect(start).toContain('alt="a > b src=\'missing.svg\'" data-src="missing.svg"');
		expect(start).toContain(
			'[asset]: https://raw.githubusercontent.com/wiki/xnorpx/keeppeek/assets/book/src/images/diagram'
		);
		expect(start).toContain('/wiki/caf%C3%A9-guide#h%C3%A9llo');
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
		await truncate(asset, 16 * 1024 * 1024 + 1);
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
		const wiki = await mkdtemp(`${scratchRoot}/checkout-`);
		scratch.push(wiki);
		const git = (...args: string[]) =>
			execFileSync('git', ['-C', wiki, ...args], {
				timeout: 10_000,
				stdio: 'pipe'
			});
		git('init', '--quiet', '--initial-branch=main');
		git('config', 'user.name', 'Wiki Test');
		git('config', 'user.email', 'wiki-test@example.invalid');
		expect(publishWiki(output, wiki, commit)).toBe(true);
		expect(publishWiki(output, wiki, commit)).toBe(false);
	}, 30_000);
});
