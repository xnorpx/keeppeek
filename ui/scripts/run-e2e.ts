import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';

type CargoArtifact = {
	reason?: string;
	manifest_path?: string;
	target?: { name?: string; kind?: string[] };
	profile?: { test?: boolean };
	executable?: string | null;
	message?: { rendered?: string };
};

const repositoryRoot = resolve(import.meta.dirname, '../..');
const executable = process.env.KEEPPEEK_AUTH_E2E_BINARY ?? buildAuthenticationFixture();
if (!existsSync(executable)) throw new Error('Authentication fixture executable is missing.');
const arguments_ = process.argv.slice(2);
if (arguments_[0] === '--') arguments_.shift();
const child = spawnSync(
	'bun',
	['x', '--no-install', 'playwright', 'test', ...arguments_],
	{
		cwd: resolve(repositoryRoot, 'ui'),
		env: { ...process.env, KEEPPEEK_AUTH_E2E_BINARY: executable },
		stdio: 'inherit',
		windowsHide: true
	}
);
if (child.error) throw child.error;
process.exit(child.status ?? 1);

function buildAuthenticationFixture(): string {
	// Build before Playwright starts Vite: Cargo also regenerates the UI's SvelteKit files.
	const features = process.platform === 'darwin' ? ['--features', 'macos-test-aws-crypto'] : [];
	const build = spawnSync(
		'cargo',
		[
			'test',
			'--locked',
			'--lib',
			'issue123_browser_fixture',
			'--no-run',
			'--message-format=json',
			...features
		],
		{
			cwd: repositoryRoot,
			encoding: 'utf8',
			maxBuffer: 16 * 1024 * 1024,
			timeout: 15 * 60 * 1000,
			windowsHide: true,
			stdio: ['ignore', 'pipe', 'inherit']
		}
	);
	const executables = new Set<string>();
	if (build.error) throw build.error;
	for (const line of build.stdout.split('\n').filter(Boolean)) {
		const artifact = JSON.parse(line) as CargoArtifact;
		if (artifact.reason === 'compiler-message' && artifact.message?.rendered) {
			process.stderr.write(artifact.message.rendered);
		}
		if (
			artifact.reason === 'compiler-artifact' &&
			artifact.manifest_path &&
			resolve(artifact.manifest_path) === resolve(repositoryRoot, 'Cargo.toml') &&
			artifact.target?.name === 'keeppeek' &&
			artifact.target.kind?.includes('lib') &&
			artifact.profile?.test &&
			typeof artifact.executable === 'string'
		)
			executables.add(artifact.executable);
	}
	if (build.status !== 0) throw new Error('Authentication fixture compilation failed.');
	if (executables.size !== 1) throw new Error('Cargo did not identify one authentication fixture.');
	return [...executables][0];
}
