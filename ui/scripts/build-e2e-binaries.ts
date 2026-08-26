import { existsSync } from 'node:fs';
import path from 'node:path';

const repositoryRoot = path.resolve(import.meta.dir, '../..');
const executableExtension = process.platform === 'win32' ? '.exe' : '';
const releaseRoot = path.join(repositoryRoot, 'target', 'release');
const mixedCodecFixtureRoot = path.join(repositoryRoot, 'target', 'e2e-mixed-codec');
const keeppeekFeatures =
	process.platform === 'darwin' ? ['--features', 'keeppeek/macos-test-aws-crypto'] : [];

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

for (const binaryName of ['keeppeek', 'test_camera']) {
	const requiredPath = binaryPath(binaryName);
	if (!existsSync(requiredPath)) throw new Error(`Cargo did not produce ${requiredPath}`);
}

await runCargo([
	'run',
	'--release',
	'-p',
	'mp4',
	'--example',
	'mixed_gop_fixture',
	'--',
	path.join(repositoryRoot, 'crates/test-camera/testdata/cc-4k-640x360-h264.mp4'),
	path.join(repositoryRoot, 'crates/test-camera/testdata/cc-4k-3840x2160-h264.mp4'),
	path.join(repositoryRoot, 'crates/test-camera/testdata/cc-4k-640x360-h265.mp4'),
	mixedCodecFixtureRoot
]);

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
