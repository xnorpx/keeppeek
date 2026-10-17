import { defineConfig } from 'vitest/config';

export default defineConfig({
	test: {
		include: [
			'scripts/wiki-*.spec.ts',
			'scripts/build-wiki*.spec.ts',
			'scripts/publish-wiki.spec.ts'
		],
		environment: 'node',
		maxWorkers: 1,
		expect: { requireAssertions: true }
	}
});
