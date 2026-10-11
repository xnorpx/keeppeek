import { describe, expect, it } from 'bun:test';
import { buildWikiManifest, parseBookSummary } from './wiki-manifest';

describe('wiki manifest', () => {
	it('covers every mdBook page and selected engineering guide', async () => {
		const summary = await Bun.file(new URL('../../book/src/SUMMARY.md', import.meta.url)).text();
		const pages = buildWikiManifest(summary);
		const bookPages = pages.filter((page) => page.kind === 'book');

		expect(bookPages.map((page) => page.source)).toEqual(
			parseBookSummary(summary).map((page) => page.source)
		);
		expect(bookPages).toHaveLength(28);
		expect(pages.some((page) => page.source === 'docs/wiki-publishing.md')).toBe(true);
		expect(new Set(pages.map((page) => page.name.toLowerCase())).size).toBe(pages.length);
	});

	it('retains nested section labels and page order', () => {
		const pages = parseBookSummary(`
# Main
- [First](./first.md)
## Nested
- [Second](./second.md)
# Next
- [Third](./third.md)
`);

		expect(pages.map(({ title, section }) => [title, section])).toEqual([
			['First', 'Main'],
			['Second', 'Main / Nested'],
			['Third', 'Next']
		]);
	});

	it('rejects duplicate basenames and case-insensitive wiki names', () => {
		expect(() => buildWikiManifest('- [One](./first/page.md)\n- [Two](./second/page.md)')).toThrow(
			/duplicate wiki page name/i
		);
		expect(() => buildWikiManifest('- [One](./Page.md)\n- [Two](./page.md)')).toThrow(
			/duplicate wiki page name/i
		);
	});

	it('rejects path traversal, unsupported SUMMARY lines, and empty books', () => {
		expect(() => parseBookSummary('- [Escape](./../../secret.md)')).toThrow(
			/invalid SUMMARY page path/i
		);
		expect(() => parseBookSummary('This is not a page entry')).toThrow(
			/unsupported SUMMARY entry/i
		);
		expect(() => parseBookSummary('# Empty\n')).toThrow(/contains no pages/i);
	});
});
