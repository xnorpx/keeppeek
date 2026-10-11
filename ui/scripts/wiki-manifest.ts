import path from 'node:path';

const MAX_WIKI_PAGES = 100;
const RESERVED_NAMES = new Set(['home', '_sidebar', 'wiki-manifest']);

export const WIKI_DOC_SOURCES = [
	'docs/backup-and-restore.md',
	'docs/camera-controls.md',
	'docs/computer-vision.md',
	'docs/configuration-management.md',
	'docs/event-forwarder.md',
	'docs/event-presentation.md',
	'docs/evidence-exports.md',
	'docs/external-authentication-operations.md',
	'docs/groups.md',
	'docs/hikvision-isapi.md',
	'docs/home-assistant.md',
	'docs/macos-installation.md',
	'docs/notifications.md',
	'docs/onvif-events.md',
	'docs/pushover.md',
	'docs/recording-integrity.md',
	'docs/recording-policy-audit.md',
	'docs/secrets.md',
	'docs/wiki-publishing.md',
	'docs/windows-service.md'
] as const;

export type WikiPage = {
	name: string;
	source: string;
	title: string;
	section: string;
	kind: 'book' | 'docs';
};

type SummaryPage = Pick<WikiPage, 'source' | 'title' | 'section'>;

function summarySourcePath(target: string, lineNumber: number): string {
	if (!target.startsWith('./') || target.includes('\\') || /[#?]/.test(target)) {
		throw new Error(`Invalid SUMMARY page path on line ${lineNumber}: ${target}`);
	}
	const relativePath = target.slice(2);
	const segments = relativePath.split('/');
	if (
		!relativePath.endsWith('.md') ||
		segments.some((segment) => !segment || segment === '.' || segment === '..')
	) {
		throw new Error(`Invalid SUMMARY page path on line ${lineNumber}: ${target}`);
	}
	return path.posix.join('book/src', relativePath);
}

export function parseBookSummary(summary: string): SummaryPage[] {
	const headings: string[] = [];
	const pages: SummaryPage[] = [];
	for (const [index, line] of summary.split(/\r?\n/).entries()) {
		if (!line.trim()) continue;
		const heading = /^(#{1,6})\s+(.+?)\s*#*\s*$/.exec(line);
		if (heading) {
			const level = heading[1].length;
			headings.length = level - 1;
			headings[level - 1] = heading[2];
			continue;
		}
		const entry = /^\s*-\s+\[([^\]]+)\]\(([^)]+)\)\s*$/.exec(line);
		if (!entry) throw new Error(`Unsupported SUMMARY entry on line ${index + 1}`);
		const title = entry[1].trim();
		if (!title) throw new Error(`Empty SUMMARY title on line ${index + 1}`);
		pages.push({
			source: summarySourcePath(entry[2], index + 1),
			title,
			section: headings.filter(Boolean).join(' / ') || 'Book'
		});
	}
	if (!pages.length) throw new Error('SUMMARY contains no pages');
	return pages;
}

function wikiName(source: string, kind: WikiPage['kind']): string {
	const stem = path.posix.basename(source, '.md');
	if (!stem || stem === 'SUMMARY') throw new Error(`Invalid wiki source path: ${source}`);
	return `${kind === 'docs' ? 'Engineering-' : ''}${stem[0].toLocaleUpperCase('en-US')}${stem.slice(1)}`;
}

function validatePages(pages: WikiPage[]): WikiPage[] {
	if (!pages.length) throw new Error('Wiki manifest contains no pages');
	if (pages.length > MAX_WIKI_PAGES) {
		throw new Error(`Wiki manifest exceeds the ${MAX_WIKI_PAGES}-page limit`);
	}
	const names = new Set<string>(RESERVED_NAMES);
	const sources = new Set<string>();
	for (const page of pages) {
		if (
			!page.name ||
			page.name.length > 100 ||
			/[\\/:*?"<>|\u0000-\u001f]/.test(page.name) ||
			page.name.trim() !== page.name ||
			page.name === '.' ||
			page.name === '..'
		) {
			throw new Error(`Invalid wiki page name: ${page.name}`);
		}
		const normalizedName = page.name.toLocaleLowerCase('en-US');
		if (names.has(normalizedName)) {
			throw new Error(`Duplicate wiki page name: ${page.name}`);
		}
		names.add(normalizedName);
		if (sources.has(page.source)) throw new Error(`Duplicate wiki source path: ${page.source}`);
		sources.add(page.source);
	}
	return pages;
}

export function buildWikiManifest(summary: string): WikiPage[] {
	const bookPages = parseBookSummary(summary).map((page) => ({
		...page,
		name: wikiName(page.source, 'book'),
		kind: 'book' as const
	}));
	const docsPages = WIKI_DOC_SOURCES.map((source) => ({
		source,
		name: wikiName(source, 'docs'),
		title: path.posix
			.basename(source, '.md')
			.split('-')
			.map((part) => part[0]?.toLocaleUpperCase('en-US') + part.slice(1))
			.join(' '),
		section: 'Engineering guides',
		kind: 'docs' as const
	}));
	return validatePages([...bookPages, ...docsPages]);
}

export function assertSupportedMarkdown(markdown: string, source: string): void {
	if (/\{\{#/.test(markdown)) {
		throw new Error(`Unsupported mdBook directive in ${source}`);
	}
}
