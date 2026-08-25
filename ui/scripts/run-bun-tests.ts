import { resolve } from 'node:path';
import { isBunCompatibleTest, normalizeTestPath } from './bun-test-selection';

const workspaceRoot = resolve(import.meta.dir, '..');
const testGlob = new Bun.Glob('src/**/*.{test,spec}.{js,ts}');
const testFiles = ['scripts/bun-test-selection.spec.ts', 'scripts/storybook-readiness.spec.ts'];

for await (const testFile of testGlob.scan({ cwd: workspaceRoot, onlyFiles: true })) {
	if (!isBunCompatibleTest(testFile)) continue;
	testFiles.push(normalizeTestPath(testFile));
}

testFiles.sort();
if (testFiles.length === 0) throw new Error('No Bun-compatible tests were found');

console.log(`Running ${testFiles.length} Bun-compatible test files in parallel`);
const testProcess = Bun.spawn(
	[process.execPath, 'test', '--parallel', '--only-failures', ...testFiles],
	{
		cwd: workspaceRoot,
		stdin: 'inherit',
		stdout: 'inherit',
		stderr: 'inherit'
	}
);

process.exitCode = await testProcess.exited;
