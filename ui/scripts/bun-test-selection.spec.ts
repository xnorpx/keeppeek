import { describe, expect, it } from 'bun:test';
import { isBunCompatibleTest, normalizeTestPath } from './bun-test-selection';

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
});
