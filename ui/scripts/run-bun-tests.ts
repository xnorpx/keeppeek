import { resolve } from 'node:path';

const workspaceRoot = resolve(import.meta.dir, '..');
const vitestOnlyTests = new Set([
	'src/lib/api.spec.ts',
	'src/lib/capability-state.spec.ts',
	'src/lib/control-client.spec.ts'
]);
const testGlob = new Bun.Glob('src/**/*.{test,spec}.{js,ts}');
const testFiles: string[] = [];

for await (const testFile of testGlob.scan({ cwd: workspaceRoot, onlyFiles: true })) {
	if (testFile.includes('.svelte.') || vitestOnlyTests.has(testFile)) continue;
	testFiles.push(testFile);
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
