import { existsSync } from 'node:fs';
import path from 'node:path';

const repositoryRoot = path.resolve(import.meta.dir, '../..');
const executableExtension = process.platform === 'win32' ? '.exe' : '';
const releaseRoot = path.join(repositoryRoot, 'target', 'release');
const keeppeekFeatures =
	process.platform === 'darwin' ? ['--features', 'keeppeek/macos-test-aws-crypto'] : [];
const force = process.argv.slice(2).includes('--force');
const requiredBinaries = ['keeppeek', 'test_camera'].map(binaryPath);

if (force || requiredBinaries.some((binary) => !existsSync(binary))) {
	await runCargo([
		'build',
		'--release',
		'-p',
		'keeppeek',
		'-p',
		'test-camera',
		'--bin',
		'keeppeek',
		'--bin',
		'test_camera',
		...keeppeekFeatures
	]);
}

for (const binaryName of ['keeppeek', 'test_camera']) {
	const requiredPath = binaryPath(binaryName);
	if (!existsSync(requiredPath)) throw new Error(`Cargo did not produce ${requiredPath}`);
}

console.log(`Release E2E binaries are ready in ${releaseRoot}`);

function binaryPath(binaryName: string): string {
	return path.join(releaseRoot, `${binaryName}${executableExtension}`);
}

async function runCargo(arguments_: string[]): Promise<void> {
	const child = Bun.spawn(['cargo', ...arguments_], {
		cwd: repositoryRoot,
		stdin: 'inherit',
		stdout: 'inherit',
		stderr: 'inherit'
	});
	const exitCode = await child.exited;
	if (exitCode !== 0) throw new Error(`cargo ${arguments_.join(' ')} exited with ${exitCode}`);
}
