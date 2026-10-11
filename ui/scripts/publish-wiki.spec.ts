import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { existsSync } from 'node:fs';
import { mkdtemp, mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {
	assertTrustedPublicationContext,
	gitLiteralPathspec,
	syncWikiDirectory
} from './publish-wiki';
import {
	wikiOwnershipMarker,
	type WikiBuildAsset,
	type WikiBuildManifest,
	type WikiBuildPage
} from './build-wiki';

const commit = '0123456789abcdef0123456789abcdef01234567';
const hash = '0'.repeat(64);
let root: string;
let wiki: string;
let generated: string;
let outside: string;

beforeEach(async () => {
	root = await mkdtemp(path.join(os.tmpdir(), 'keeppeek-wiki-publish-'));
	wiki = path.join(root, 'wiki');
	generated = path.join(root, 'generated');
	outside = path.join(root, 'outside');
	await Promise.all([mkdir(wiki), mkdir(generated), mkdir(outside)]);
});

afterEach(async () => {
	await rm(root, { recursive: true, force: true });
});

function page(source: string, name: string): WikiBuildPage {
	return { source, name, title: name, section: 'Book', kind: 'book', sha256: hash };
}

function asset(source: string): WikiBuildAsset {
	return { source, destination: `assets/${source}`, sha256: hash };
}

function manifest(pages: WikiBuildPage[], assets: WikiBuildAsset[] = []): WikiBuildManifest {
	return {
		formatVersion: 1,
		sourceCommit: commit,
		summarySha256: hash,
		pages,
		assets,
		outputs: [
			'Home.md',
			'_Sidebar.md',
			'Wiki-Manifest.json',
			...pages.map((item) => `${item.name}.md`),
			...assets.map((item) => item.destination)
		].sort()
	};
}

async function writeTree(
	directory: string,
	value: WikiBuildManifest,
	content: string
): Promise<void> {
	for (const output of value.outputs) {
		if (output === 'Wiki-Manifest.json') {
			await writeFile(path.join(directory, output), `${JSON.stringify(value)}\n`);
		} else {
			const target = path.join(directory, output);
			await mkdir(path.dirname(target), { recursive: true });
			const source =
				value.pages.find((item) => `${item.name}.md` === output)?.source ??
				(output === 'Home.md' || output === '_Sidebar.md' ? output : undefined);
			const marker = source ? wikiOwnershipMarker(source, value.sourceCommit) : '';
			await writeFile(target, output.startsWith('assets/') ? '<svg />\n' : `${marker}${content}`);
		}
	}
}

describe('wiki publication', () => {
	it('replaces only manifest-owned output and preserves manual pages', async () => {
		const old = manifest([page('book/src/old.md', 'Old')], [asset('book/src/images/old.svg')]);
		const next = manifest([page('book/src/new.md', 'New')]);
		await writeTree(wiki, old, 'old generated page\n');
		await writeFile(path.join(wiki, 'Manual.md'), 'manual page\n');
		await writeTree(generated, next, 'new generated page\n');

		const changed = syncWikiDirectory(wiki, generated, commit);

		expect(changed).toContain('Old.md');
		expect(changed).toContain('assets/book/src/images/old.svg');
		expect(await readFile(path.join(wiki, 'Manual.md'), 'utf8')).toBe('manual page\n');
		expect(await readFile(path.join(wiki, 'New.md'), 'utf8')).toContain('new generated page\n');
		expect(existsSync(path.join(wiki, 'Old.md'))).toBe(false);
	});

	it('fails on unowned page conflicts before changing the wiki', async () => {
		const next = manifest([page('book/src/new.md', 'New')]);
		await writeFile(path.join(wiki, 'New.md'), 'manual page\n');
		await writeTree(generated, next, 'generated page\n');

		expect(() => syncWikiDirectory(wiki, generated, commit)).toThrow(
			/conflicts with an unowned page/i
		);
		expect(await readFile(path.join(wiki, 'New.md'), 'utf8')).toBe('manual page\n');
		expect(existsSync(path.join(wiki, 'Home.md'))).toBe(false);
	});

	it('does not trust a manifest that claims an unmarked manual page', async () => {
		const forged = manifest([page('book/src/manual.md', 'Manual')]);
		await writeTree(wiki, forged, 'generated looking page\n');
		await writeFile(path.join(wiki, 'Manual.md'), 'manual page\n');
		const next = manifest([page('book/src/new.md', 'New')]);
		await writeTree(generated, next, 'new generated page\n');

		expect(() => syncWikiDirectory(wiki, generated, commit)).toThrow(/ownership marker/i);
		expect(await readFile(path.join(wiki, 'Manual.md'), 'utf8')).toBe('manual page\n');
		expect(await readFile(path.join(wiki, 'Home.md'), 'utf8')).toContain(
			'generated looking page\n'
		);
	});

	it('rejects malformed ownership paths without deleting files outside the wiki', async () => {
		const old = manifest([page('book/src/old.md', 'Old')]);
		await writeTree(wiki, old, 'old generated page\n');
		await writeFile(path.join(outside, 'sentinel'), 'keep\n');
		const malformed = { ...old, outputs: [...old.outputs, '../../outside/sentinel'] };
		await writeFile(path.join(wiki, 'Wiki-Manifest.json'), JSON.stringify(malformed));
		const next = manifest([page('book/src/new.md', 'New')]);
		await writeTree(generated, next, 'new generated page\n');

		expect(() => syncWikiDirectory(wiki, generated, commit)).toThrow(/output list/i);
		expect(await readFile(path.join(outside, 'sentinel'), 'utf8')).toBe('keep\n');
		expect(await readFile(path.join(wiki, 'Old.md'), 'utf8')).toContain('old generated page\n');
	});

	it('rejects symlinked output parents before mutation', async () => {
		const old = manifest([page('book/src/old.md', 'Old')], [asset('book/src/images/old.svg')]);
		await writeTree(wiki, old, 'old generated page\n');
		await rm(path.join(wiki, 'assets'), { recursive: true });
		await writeFile(path.join(outside, 'old.svg'), 'external\n');
		await symlink(outside, path.join(wiki, 'assets'));
		const next = manifest([page('book/src/new.md', 'New')]);
		await writeTree(generated, next, 'new generated page\n');

		expect(() => syncWikiDirectory(wiki, generated, commit)).toThrow(/symbolic link/i);
		expect(await readFile(path.join(wiki, 'Home.md'), 'utf8')).toContain('old generated page\n');
		expect(await readFile(path.join(wiki, 'Old.md'), 'utf8')).toContain('old generated page\n');
		expect(await readFile(path.join(outside, 'old.svg'), 'utf8')).toBe('external\n');
	});

	it('returns no changes when generated output already matches', async () => {
		const value = manifest([page('book/src/start.md', 'Start')]);
		await writeTree(wiki, value, 'generated page\n');
		await writeTree(generated, value, 'generated page\n');

		expect(syncWikiDirectory(wiki, generated, commit)).toEqual([]);
	});

	it('rejects an outdated build before replacing any Wiki files', async () => {
		const value = manifest([page('book/src/start.md', 'Start')]);
		await writeTree(generated, value, 'new content\n');
		await writeFile(path.join(wiki, 'Home.md'), 'manual home\n');

		expect(() =>
			syncWikiDirectory(wiki, generated, 'ffffffffffffffffffffffffffffffffffffffff')
		).toThrow(/does not match/i);
		expect(await readFile(path.join(wiki, 'Home.md'), 'utf8')).toBe('manual home\n');
	});

	it('uses literal Git pathspecs for generated asset names', () => {
		expect(gitLiteralPathspec('assets/docs/diagram*.svg')).toBe(
			':(literal)assets/docs/diagram*.svg'
		);
	});

	it('allows only trusted main-branch publication contexts', () => {
		expect(() =>
			assertTrustedPublicationContext({
				repository: 'xnorpx/keeppeek',
				ref: 'refs/heads/main',
				event: 'push'
			})
		).not.toThrow();
		expect(() =>
			assertTrustedPublicationContext({
				repository: 'xnorpx/keeppeek',
				ref: 'refs/heads/main',
				event: 'workflow_dispatch'
			})
		).not.toThrow();
		for (const context of [
			{ repository: 'fork/keeppeek', ref: 'refs/heads/main', event: 'push' },
			{ repository: 'xnorpx/keeppeek', ref: 'refs/heads/feature', event: 'push' },
			{ repository: 'xnorpx/keeppeek', ref: 'refs/pull/3/merge', event: 'pull_request' }
		]) {
			expect(() => assertTrustedPublicationContext(context)).toThrow(/restricted|does not allow/i);
		}
	});
});
