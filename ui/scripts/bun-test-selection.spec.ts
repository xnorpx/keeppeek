import { describe, expect, it } from 'bun:test';
import { isBunCompatibleTest, isVitestCompatTest, normalizeTestPath } from './bun-test-selection';

describe('Bun test selection', () => {
	it('normalizes Windows paths before applying exclusions', () => {
		expect(normalizeTestPath('src\\lib\\camera.spec.ts')).toBe('src/lib/camera.spec.ts');
		for (const testFile of [
			'src\\lib\\api.spec.ts',
			'src\\lib\\capability-state.spec.ts',
			'src\\lib\\control-client.spec.ts'
		]) {
			expect(isBunCompatibleTest(testFile)).toBe(false);
		}
	});

	it('keeps compatible tests and excludes Svelte specs on either platform', () => {
		expect(isBunCompatibleTest('src/lib/camera.spec.ts')).toBe(true);
		expect(isBunCompatibleTest('src\\lib\\components\\Camera.svelte.spec.ts')).toBe(false);
	});

	it('assigns every source test file to exactly one CI unit owner', async () => {
		const testGlob = new Bun.Glob('src/**/*.{test,spec}.{js,ts}');
		for await (const testFile of testGlob.scan({ cwd: import.meta.dir + '/..', onlyFiles: true })) {
			const normalized = normalizeTestPath(testFile);
			const isSvelteTest = normalized.includes('.svelte.');
			const owners = [
				isBunCompatibleTest(normalized),
				isVitestCompatTest(normalized),
				isSvelteTest && normalized.includes('.story.svelte.spec.'),
				isSvelteTest &&
					!normalized.includes('.story.svelte.spec.') &&
					!normalized.startsWith('src/lib/server/')
			].filter(Boolean);
			expect(owners, `${normalized} must have exactly one CI unit owner`).toHaveLength(1);
		}
	});
});
