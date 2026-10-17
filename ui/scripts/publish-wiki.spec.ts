import { afterEach, beforeEach, describe, expect, test } from 'vitest';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	symlinkSync,
	writeFileSync
} from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { publishWiki } from './publish-wiki';

const sourceCommit = 'a'.repeat(40);
let root: string;
let generated: string;
let wiki: string;
let remote: string;

function git(directory: string, ...args: string[]): string {
	return execFileSync('git', ['-C', directory, ...args], {
		encoding: 'utf8',
		timeout: 10_000,
		env: {
			...process.env,
			GIT_AUTHOR_NAME: 'Wiki test',
			GIT_AUTHOR_EMAIL: 'wiki-test@example.invalid',
			GIT_COMMITTER_NAME: 'Wiki test',
			GIT_COMMITTER_EMAIL: 'wiki-test@example.invalid'
		},
		stdio: ['ignore', 'pipe', 'pipe']
	}).trim();
}

function generate(files: Record<string, string> = { 'Home.md': '# Home\n' }): void {
	rmSync(generated, { recursive: true, force: true });
	mkdirSync(generated);
	for (const [path, content] of Object.entries(files)) {
		mkdirSync(dirname(join(generated, path)), { recursive: true });
		writeFileSync(join(generated, path), content);
	}
	writeFileSync(
		join(generated, '.wiki-manifest.json'),
		JSON.stringify({
			schemaVersion: 1,
			sourceCommit,
			pages: { 'book/src/README.md': 'Home' },
			files: Object.fromEntries(
				Object.entries(files).map(([path, content]) => [
					path,
					createHash('sha256').update(content).digest('hex')
				])
			)
		})
	);
}

function editManifest(
	edit: (manifest: Record<string, unknown>) => void,
	directory = generated
): void {
	const path = join(directory, '.wiki-manifest.json');
	const manifest = JSON.parse(readFileSync(path, 'utf8')) as Record<string, unknown>;
	edit(manifest);
	writeFileSync(path, JSON.stringify(manifest));
}

function commitManual(path: string, content: string): void {
	mkdirSync(dirname(join(wiki, path)), { recursive: true });
	writeFileSync(join(wiki, path), content);
	git(wiki, 'add', '--', path);
	git(wiki, '-c', 'core.hooksPath=/dev/null', 'commit', '-m', 'Manual edit');
}

beforeEach(() => {
	root = mkdtempSync(resolve('.wiki-publisher-test-'));
	generated = join(root, 'generated');
	wiki = join(root, 'wiki');
	remote = join(root, 'remote.git');
	mkdirSync(remote);
	git(remote, 'init', '--bare', '--initial-branch=main');
	git(root, 'clone', remote, wiki);
	git(wiki, 'config', 'user.name', 'Wiki publisher test');
	git(wiki, 'config', 'user.email', 'wiki-test@example.invalid');
	generate();
});

afterEach(() => {
	rmSync(root, { recursive: true, force: true });
});

describe('publishWiki', () => {
	test('commits owned files with source provenance and skips identical output', () => {
		expect(publishWiki(generated, wiki)).toBe(true);
		const commit = git(wiki, 'rev-parse', 'HEAD');
		expect(git(wiki, 'log', '-1', '--format=%B')).toContain(sourceCommit);
		expect(publishWiki(generated, wiki)).toBe(false);
		expect(git(wiki, 'rev-parse', 'HEAD')).toBe(commit);
		expect(git(wiki, 'status', '--porcelain')).toBe('');
	});

	test('runs the CLI without pushing and fails invalid arguments', () => {
		const script = resolve(import.meta.dirname, 'publish-wiki.ts');
		const output = execFileSync('bun', [script, generated, wiki], {
			encoding: 'utf8',
			timeout: 10_000,
			env: { ...process.env, GITHUB_SHA: sourceCommit }
		});
		expect(output).toContain('Wiki commit created.');
		expect(git(wiki, 'log', '-1', '--format=%an <%ae>')).toBe(
			'Wiki publisher test <wiki-test@example.invalid>'
		);
		expect(() => git(root, '--git-dir', remote, 'rev-parse', '--verify', 'HEAD')).toThrow();
		expect(() => execFileSync('bun', [script], { timeout: 10_000, stdio: 'pipe' })).toThrow();
	});

	test.each(['b'.repeat(40), '', 'invalid-sha'])(
		'refuses a CLI artifact that does not match GITHUB_SHA=%s without writes',
		(expectedCommit) => {
			expect(() =>
				execFileSync('bun', [resolve(import.meta.dirname, 'publish-wiki.ts'), generated, wiki], {
					timeout: 10_000,
					stdio: 'pipe',
					env: { ...process.env, GITHUB_SHA: expectedCommit }
				})
			).toThrow(/GITHUB_SHA|source commit/);
			expect(existsSync(join(wiki, 'Home.md'))).toBe(false);
			expect(existsSync(join(wiki, '.wiki-manifest.json'))).toBe(false);
			expect(git(wiki, 'status', '--porcelain')).toBe('');
		}
	);

	test('removes only obsolete owned files and preserves manual pages', () => {
		generate({ 'Home.md': '# Home\n', 'assets/old.svg': '<svg/>' });
		publishWiki(generated, wiki);
		commitManual('Manual.md', 'Keep this');
		generate({ 'Home.md': '# Updated\n', 'assets/new.svg': '<svg>new</svg>' });
		expect(publishWiki(generated, wiki)).toBe(true);
		expect(existsSync(join(wiki, 'assets/old.svg'))).toBe(false);
		expect(readFileSync(join(wiki, 'Manual.md'), 'utf8')).toBe('Keep this');
		expect(git(wiki, 'diff-tree', '--no-commit-id', '--name-only', '-r', 'HEAD')).not.toContain(
			'Manual.md'
		);
	});

	test('refuses a manual collision without changing the checkout', () => {
		commitManual('Home.md', 'Manual home');
		const head = git(wiki, 'rev-parse', 'HEAD');
		expect(() => publishWiki(generated, wiki)).toThrow(/collision/i);
		expect(readFileSync(join(wiki, 'Home.md'), 'utf8')).toBe('Manual home');
		expect(git(wiki, 'rev-parse', 'HEAD')).toBe(head);
		expect(git(wiki, 'status', '--porcelain')).toBe('');
	});

	test.each(['home.md', 'HOME.md'])(
		'refuses a manual Wiki title collision with %s on case-sensitive filesystems',
		(path) => {
			commitManual(path, 'Manual home');
			expect(() => publishWiki(generated, wiki)).toThrow(/collision/i);
			expect(readFileSync(join(wiki, path), 'utf8')).toBe('Manual home');
			expect(existsSync(join(wiki, 'Home.md'))).toBe(false);
			expect(git(wiki, 'status', '--porcelain')).toBe('');
		}
	);

	test('refuses case-insensitive collisions in asset parent directories', () => {
		commitManual('Assets/manual.svg', '<svg>Manual</svg>');
		generate({ 'Home.md': '# Home\n', 'assets/new.svg': '<svg>Generated</svg>' });
		expect(() => publishWiki(generated, wiki)).toThrow(/collision/i);
		expect(existsSync(join(wiki, 'Home.md'))).toBe(false);
		expect(readFileSync(join(wiki, 'Assets/manual.svg'), 'utf8')).toBe('<svg>Manual</svg>');
	});

	test('rejects incoming paths with differently cased names for the same directory', () => {
		generate({
			'Home.md': '# Home\n',
			'assets/first.svg': '<svg/>',
			'Assets/second.svg': '<svg/>'
		});
		expect(() => publishWiki(generated, wiki)).toThrow(/collision/i);
		expect(existsSync(join(wiki, 'Home.md'))).toBe(false);
	});

	test('accepts Unicode pages and source paths plus asset paths with spaces', () => {
		generate({
			'Home.md': '# Home\n',
			'日本語-ガイド.md': '# 日本語\n',
			'assets/image name.svg': '<svg/>'
		});
		editManifest((manifest) => {
			manifest.pages = {
				'book/src/README.md': 'Home',
				'book/src/日本語 ガイド.md': '日本語-ガイド'
			};
		});
		expect(publishWiki(generated, wiki)).toBe(true);
		expect(readFileSync(join(wiki, '日本語-ガイド.md'), 'utf8')).toBe('# 日本語\n');
		expect(readFileSync(join(wiki, 'assets/image name.svg'), 'utf8')).toBe('<svg/>');
	});

	test('adopts a byte-identical initial Home page', () => {
		commitManual('Home.md', '# Home\n');
		expect(publishWiki(generated, wiki)).toBe(true);
		expect(existsSync(join(wiki, '.wiki-manifest.json'))).toBe(true);
	});

	test('refuses committed manual edits to previously generated files', () => {
		publishWiki(generated, wiki);
		commitManual('Home.md', 'Edited by hand');
		generate({ 'Home.md': '# New generated content\n' });
		expect(() => publishWiki(generated, wiki)).toThrow(/modified/i);
		expect(readFileSync(join(wiki, 'Home.md'), 'utf8')).toBe('Edited by hand');
		expect(git(wiki, 'status', '--porcelain')).toBe('');
	});

	test.each([
		'../outside.md',
		'/absolute.md',
		'.git/config',
		'.gitattributes',
		'assets/../escape',
		'x\\y',
		'x/.GIT/a',
		'x\nname',
		'x\0name'
	])('rejects unsafe manifest path %s before writing', (path) => {
		editManifest((manifest) => {
			manifest.files = { [path]: '0'.repeat(64) };
		});
		expect(() => publishWiki(generated, wiki)).toThrow(/path/i);
		expect(existsSync(join(wiki, 'Home.md'))).toBe(false);
	});

	test.each([
		['schemaVersion', 2],
		['sourceCommit', 'HEAD; echo unsafe'],
		['sourceCommit', 'abc'],
		['files', { 'Home.md': 'wrong' }],
		['pages', { '../escape.md': 'Home' }],
		['pages', { 'book/src/README.md': '../Home' }]
	])('rejects invalid %s', (key, value) => {
		editManifest((manifest) => {
			manifest[key as string] = value;
		});
		expect(() => publishWiki(generated, wiki)).toThrow();
		expect(existsSync(join(wiki, 'Home.md'))).toBe(false);
	});

	test('rejects a digest mismatch, missing file, or unowned extra output', () => {
		writeFileSync(join(generated, 'Home.md'), 'Tampered');
		expect(() => publishWiki(generated, wiki)).toThrow(/hash/i);
		generate();
		rmSync(join(generated, 'Home.md'));
		expect(() => publishWiki(generated, wiki)).toThrow();
		generate();
		writeFileSync(join(generated, 'Extra.md'), 'Unexpected');
		expect(() => publishWiki(generated, wiki)).toThrow(/unowned|extra/i);
		expect(existsSync(join(wiki, '.wiki-manifest.json'))).toBe(false);
	});

	test('rejects count, file size, and manifest size limit violations', () => {
		editManifest((manifest) => {
			manifest.files = Object.fromEntries(
				Array.from({ length: 1025 }, (_, index) => [`Page-${index}.md`, '0'.repeat(64)])
			);
		});
		expect(() => publishWiki(generated, wiki)).toThrow(/count/i);
		generate();
		writeFileSync(join(generated, 'Home.md'), Buffer.alloc(32 * 1024 * 1024 + 1));
		expect(() => publishWiki(generated, wiki)).toThrow(/size/i);
		writeFileSync(join(generated, '.wiki-manifest.json'), ' '.repeat(1024 * 1024 + 1));
		expect(() => publishWiki(generated, wiki)).toThrow(/size/i);
		expect(existsSync(join(wiki, 'Home.md'))).toBe(false);
	});

	test('rejects a committed deletion of an obsolete owned file', () => {
		generate({ 'Home.md': '# Home\n', 'Old.md': 'Old' });
		publishWiki(generated, wiki);
		git(wiki, 'rm', 'Old.md');
		git(wiki, 'commit', '-m', 'Manual deletion');
		generate();
		expect(() => publishWiki(generated, wiki)).toThrow(/missing/i);
		expect(readFileSync(join(wiki, 'Home.md'), 'utf8')).toBe('# Home\n');
	});

	test('rejects generated symlinks and checkout ancestor symlinks', () => {
		rmSync(join(generated, 'Home.md'));
		symlinkSync(join(root, 'remote.git'), join(generated, 'Home.md'));
		expect(() => publishWiki(generated, wiki)).toThrow(/symlink|regular/i);
		generate({ 'Home.md': '# Home\n', 'assets/image.svg': '<svg/>' });
		symlinkSync(remote, join(wiki, 'assets'));
		git(wiki, 'add', 'assets');
		git(wiki, 'commit', '-m', 'Manual symlink');
		expect(() => publishWiki(generated, wiki)).toThrow(/symlink|directory/i);
		expect(existsSync(join(remote, 'image.svg'))).toBe(false);
		expect(existsSync(join(wiki, 'Home.md'))).toBe(false);
	});

	test('rejects a malformed previous manifest before writing', () => {
		publishWiki(generated, wiki);
		editManifest((manifest) => {
			manifest.files = { '../outside.md': '0'.repeat(64) };
		}, wiki);
		git(wiki, 'add', '.wiki-manifest.json');
		git(wiki, 'commit', '-m', 'Broken manifest');
		generate({ 'Home.md': '# New home\n' });
		expect(() => publishWiki(generated, wiki)).toThrow(/path/i);
		expect(readFileSync(join(wiki, 'Home.md'), 'utf8')).toBe('# Home\n');
	});

	test.each(['untracked', 'unstaged', 'staged'])('requires a clean %s checkout', (kind) => {
		publishWiki(generated, wiki);
		if (kind === 'untracked') writeFileSync(join(wiki, 'Manual.md'), 'Untracked');
		else {
			writeFileSync(join(wiki, 'Home.md'), 'Dirty');
			if (kind === 'staged') git(wiki, 'add', 'Home.md');
		}
		generate({ 'Home.md': '# Updated\n' });
		expect(() => publishWiki(generated, wiki)).toThrow(/clean/i);
	});

	test('rejects overlapping roots and a non-root git checkout', () => {
		expect(() => publishWiki(generated, generated)).toThrow(/overlap|distinct/i);
		mkdirSync(join(wiki, 'nested'));
		expect(() => publishWiki(generated, join(wiki, 'nested'))).toThrow(/root/i);
		expect(() => publishWiki(root, wiki)).toThrow(/overlap|distinct/i);
	});

	test('disables local commit hooks', () => {
		writeFileSync(join(wiki, '.git/hooks/pre-commit'), '#!/bin/sh\nexit 1\n', { mode: 0o755 });
		expect(publishWiki(generated, wiki)).toBe(true);
	});

	test('leaves remote history intact when a normal push is stale or denied', () => {
		publishWiki(generated, wiki);
		git(wiki, 'push', 'origin', 'HEAD');
		const other = join(root, 'other');
		git(root, 'clone', remote, other);
		generate({ 'Home.md': '# Publisher update\n' });
		publishWiki(generated, wiki);
		writeFileSync(join(other, 'Manual.md'), 'Concurrent manual page');
		git(other, 'add', 'Manual.md');
		git(other, 'commit', '-m', 'Concurrent edit');
		git(other, 'push', 'origin', 'HEAD');
		const remoteHead = git(root, '--git-dir', remote, 'rev-parse', 'HEAD');
		expect(() => git(wiki, 'push', 'origin', 'HEAD')).toThrow();
		expect(git(root, '--git-dir', remote, 'rev-parse', 'HEAD')).toBe(remoteHead);
		writeFileSync(join(remote, 'hooks/pre-receive'), '#!/bin/sh\nexit 1\n', { mode: 0o755 });
		expect(() => git(other, 'push', 'origin', 'HEAD:refs/heads/denied')).toThrow();
		expect(git(root, '--git-dir', remote, 'rev-parse', 'HEAD')).toBe(remoteHead);
	});
});

type WorkflowStep = {
	name?: string;
	uses?: string;
	run?: string;
	env?: Record<string, string>;
	with?: Record<string, unknown>;
};

type WorkflowJob = {
	if: string;
	needs?: string;
	environment?: string;
	permissions?: Record<string, string>;
	env?: Record<string, string>;
	steps: WorkflowStep[];
};

function publicationWorkflow(): {
	on: Record<string, unknown>;
	permissions: Record<string, string>;
	env?: Record<string, string>;
	jobs: Record<string, WorkflowJob>;
} {
	const yaml = readFileSync(
		resolve(import.meta.dirname, '../../.github/workflows/publish-wiki.yml'),
		'utf8'
	);
	return JSON.parse(
		execFileSync(
			'bun',
			['--eval', 'console.log(JSON.stringify(Bun.YAML.parse(await Bun.stdin.text())))'],
			{ input: yaml, encoding: 'utf8', timeout: 10_000 }
		)
	);
}

describe('Wiki publication workflow trust boundary', () => {
	test('allows only main pushes and manual dispatch in the exact upstream repository', () => {
		const workflow = publicationWorkflow();
		expect(Object.keys(workflow.on).sort()).toEqual(['push', 'workflow_dispatch']);
		expect(workflow.on.push).toMatchObject({ branches: ['main'] });
		const guard =
			"github.repository == 'xnorpx/keeppeek' && github.ref == 'refs/heads/main' && " +
			"(github.event_name == 'push' || github.event_name == 'workflow_dispatch')";
		for (const job of Object.values(workflow.jobs)) {
			expect(job.if.trim().replace(/\s+/g, ' ')).toBe(guard);
		}
		expect(JSON.stringify(workflow)).not.toContain('pull_request_target');
	});

	test('uses read-only built-in permissions without persistent checkout credentials', () => {
		const workflow = publicationWorkflow();
		expect(workflow.permissions).toEqual({ contents: 'read' });
		for (const job of Object.values(workflow.jobs)) {
			expect(job.permissions === undefined || job.permissions.contents === 'read').toBe(true);
			const checkout = job.steps.find((step) => step.uses?.startsWith('actions/checkout@'));
			expect(checkout?.with?.['persist-credentials']).toBe(false);
		}
		expect(JSON.stringify(workflow)).not.toContain('write-all');
	});

	test('builds and validates separately before the protected publication job', () => {
		const { jobs } = publicationWorkflow();
		expect(jobs.publish.needs).toBe('build');
		expect(jobs.publish.environment).toBe('wiki');
		const build = jobs.build.steps.map((step) => step.run ?? '').join('\n');
		expect(build).toContain('bun run wiki:typecheck');
		expect(build).toContain('bun run wiki:test');
		expect(build).toContain('bun run wiki:build');
		const upload = jobs.build.steps.find((step) =>
			step.uses?.startsWith('actions/upload-artifact@')
		);
		const download = jobs.publish.steps.find((step) =>
			step.uses?.startsWith('actions/download-artifact@')
		);
		expect(upload?.with?.['include-hidden-files']).toBe(true);
		expect(upload?.with?.name).toBe(download?.with?.name);
		expect(upload?.with?.['if-no-files-found']).toBe('error');
	});

	test('exposes the publishing secret only to the final shell step', () => {
		const workflow = publicationWorkflow();
		const publish = workflow.jobs.publish;
		expect(JSON.stringify(workflow.env ?? {})).not.toMatch(/secrets\.|WIKI_PUBLISH_TOKEN/);
		for (const [name, job] of Object.entries(workflow.jobs)) {
			expect(JSON.stringify(job.env ?? {})).not.toMatch(/secrets\.|WIKI_PUBLISH_TOKEN/);
			const uncredentialed = name === 'publish' ? job.steps.slice(0, -1) : job.steps;
			expect(JSON.stringify(uncredentialed)).not.toMatch(/secrets\.|WIKI_PUBLISH_TOKEN/);
		}
		const final = publish.steps.at(-1)!;
		expect(final.env?.WIKI_PUBLISH_TOKEN).toBe('${{ secrets.WIKI_PUBLISH_TOKEN }}');
		expect(final.uses).toBeUndefined();
		expect(final.run).toContain('if [ -z "$WIKI_PUBLISH_TOKEN" ]');
		expect(final.run).not.toMatch(/bun (?:install|run wiki:build)|npm |npx /);
	});

	test('clones with scoped credentials and invokes the local CLI before a normal push', () => {
		const final = publicationWorkflow().jobs.publish.steps.at(-1)!;
		expect(final.env?.GIT_CONFIG_KEY_0).toBe('credential.helper');
		expect(final.env?.GIT_CONFIG_VALUE_0).toBe('');
		expect(final.env?.GIT_CONFIG_KEY_1).toBe('credential.https://github.com.helper');
		expect(final.env?.GIT_CONFIG_VALUE_1).toContain('password "$WIKI_PUBLISH_TOKEN"');
		const shell = final.run!;
		expect(shell).toContain(
			'git clone -- https://github.com/xnorpx/keeppeek.wiki.git "$RUNNER_TEMP/wiki-checkout"'
		);
		expect(shell).toContain("config user.name 'KeepPeek documentation'");
		expect(shell).toContain("config user.email 'keeppeek-wiki@users.noreply.github.com'");
		const command =
			'bun ui/scripts/publish-wiki.ts "$RUNNER_TEMP/wiki-generated" "$RUNNER_TEMP/wiki-checkout"';
		const push = 'git -C "$RUNNER_TEMP/wiki-checkout" push origin HEAD';
		expect(shell).toContain(command);
		expect(shell).toContain(push);
		expect(shell).toContain(
			'if [ "$previous_head" != "$(git -C "$RUNNER_TEMP/wiki-checkout" rev-parse HEAD)" ]; then'
		);
		expect(shell.indexOf(command)).toBeLessThan(shell.indexOf(push));
		expect(shell).not.toMatch(/--force|-f(?:\s|$)|--mirror|https:\/\/[^/\s]+@/);
	});
});
