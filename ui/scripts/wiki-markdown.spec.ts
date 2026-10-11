import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { mkdtemp, mkdir, rm, symlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { convertMarkdown } from './wiki-markdown';
import type { WikiPage } from './wiki-manifest';

let root: string;
let outside: string;

const currentPage: WikiPage = {
	name: 'Current',
	source: 'book/src/current.md',
	title: 'Current',
	section: 'Book',
	kind: 'book'
};
const relatedPage: WikiPage = {
	name: 'Related',
	source: 'book/src/related file.md',
	title: 'Related',
	section: 'Book',
	kind: 'book'
};
const guidePage: WikiPage = {
	name: 'Engineering-Security-Guide',
	source: 'docs/security guide.md',
	title: 'Security guide',
	section: 'Engineering guides',
	kind: 'docs'
};
const pages = new Map([
	[currentPage.source, currentPage],
	[relatedPage.source, relatedPage],
	[guidePage.source, guidePage]
]);

beforeEach(async () => {
	root = await mkdtemp(path.join(os.tmpdir(), 'keeppeek-wiki-'));
	outside = await mkdtemp(path.join(os.tmpdir(), 'keeppeek-wiki-outside-'));
	await mkdir(path.join(root, 'book/src/images'), { recursive: true });
	await mkdir(path.join(root, 'docs'), { recursive: true });
	await mkdir(path.join(root, 'api'), { recursive: true });
	await writeFile(path.join(root, relatedPage.source), '# Related\n');
	await writeFile(path.join(root, guidePage.source), '# Security guide\n');
	await writeFile(path.join(root, 'api/README.md'), '# API\n');
	await writeFile(path.join(root, 'book/src/images/naïve image.svg'), '<svg />\n');
});

afterEach(async () => {
	await Promise.all([
		rm(root, { recursive: true, force: true }),
		rm(outside, { recursive: true, force: true })
	]);
});

function convert(markdown: string, source = currentPage): ReturnType<typeof convertMarkdown> {
	return convertMarkdown(markdown, {
		repositoryRoot: root,
		sourceCommit: '0123456789abcdef0123456789abcdef01234567',
		page: source,
		pages
	});
}

describe('wiki markdown conversion', () => {
	it('rewrites local pages, references, tables, and images without changing code fences', () => {
		const mermaid = '```mermaid\nflowchart TD\n  A --> B\n```\n';
		const code = '```md\n[not a link](./missing.md)\n```\n';
		const markdown = [
			'# Unicode 😀 heading',
			'[Related](./related%20file.md#A-section)',
			'[Guide][ops]',
			'',
			'[ops]: ../../docs/security%20guide.md#Recovery',
			'![Diagram](./images/na%C3%AFve%20image.svg "Source diagram")',
			'| [Table link](./related%20file.md#table) |',
			mermaid.trimEnd(),
			code.trimEnd(),
			'[External](https://example.com/guide)',
			'<https://pushover.net/apps/build>',
			'[Local heading](#unicode-heading)'
		].join('\n');

		const result = convert(markdown);

		expect(result.markdown).toContain('[Related](Related#A-section)');
		expect(result.markdown).toContain('[ops]: Engineering-Security-Guide#Recovery');
		expect(result.markdown).toContain(
			'![Diagram](assets/book/src/images/na%C3%AFve%20image.svg "Source diagram")'
		);
		expect(result.markdown).toContain('| [Table link](Related#table) |');
		expect(result.markdown).toContain(mermaid);
		expect(result.markdown).toContain(code);
		expect(result.markdown).toContain('[External](https://example.com/guide)');
		expect(result.markdown).toContain('<https://pushover.net/apps/build>');
		expect(result.markdown).toContain('[Local heading](#unicode-heading)');
		expect(result.assets).toEqual([
			{
				source: 'book/src/images/naïve image.svg',
				destination: 'assets/book/src/images/naïve image.svg'
			}
		]);
	});

	it('rewrites repository links to selected pages and pins other files to the source commit', () => {
		const markdown = [
			'[Book](https://github.com/xnorpx/keeppeek/blob/main/book/src/related%20file.md#part)',
			'<https://github.com/xnorpx/keeppeek/blob/main/book/src/related%20file.md#autolink>',
			'[API](../../api/README.md)'
		].join('\n');

		const result = convert(markdown);

		expect(result.markdown).toContain('[Book](Related#part)');
		expect(result.markdown).toContain(
			'[https://github.com/xnorpx/keeppeek/blob/main/book/src/related%20file.md#autolink](Related#autolink)'
		);
		expect(result.markdown).toContain(
			'[API](https://github.com/xnorpx/keeppeek/blob/0123456789abcdef0123456789abcdef01234567/api/README.md)'
		);
	});

	it('preserves external HTML media and rejects relative HTML assets', () => {
		const media = '<video><source src="https://example.com/demo.mp4"></video>';
		expect(convert(media).markdown).toBe(media);
		expect(() => convert('<img src="./images/local.svg">')).toThrow(
			/unsupported relative HTML link or asset/i
		);
	});

	it('rejects missing targets, root traversal, and symlink escapes', async () => {
		expect(() => convert('[Missing](./missing.md)')).toThrow(/missing local target/i);
		expect(() => convert('[Escape](../../../../etc/passwd)')).toThrow(/escapes the repository/i);

		await writeFile(path.join(outside, 'secret.svg'), '<svg />\n');
		await symlink(path.join(outside, 'secret.svg'), path.join(root, 'book/src/images/escape.svg'));
		expect(() => convert('![Escape](./images/escape.svg)')).toThrow(/escapes the repository/i);
	});
});
