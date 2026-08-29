const vitestOnlyTests = new Set([
	'src/lib/api.spec.ts',
	'src/lib/capability-state.spec.ts',
	'src/lib/control-client.spec.ts'
]);

export function normalizeTestPath(testFile: string): string {
	return testFile.replaceAll('\\', '/');
}

export function isVitestCompatTest(testFile: string): boolean {
	return vitestOnlyTests.has(normalizeTestPath(testFile));
}

export function isBunCompatibleTest(testFile: string): boolean {
	const normalized = normalizeTestPath(testFile);
	return !normalized.includes('.svelte.') && !isVitestCompatTest(normalized);
}
