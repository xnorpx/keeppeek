import { spawnSync } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { expect, it } from 'vitest';

type ListedSuite = {
	file: string;
};

it('collects only the timeline workload for the static timeline server', () => {
	const uiRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
	const result = spawnSync(
		process.execPath,
		[
			resolve(uiRoot, 'node_modules/@playwright/test/cli.js'),
			'test',
			'--config',
			'playwright.timeline-performance.config.ts',
			'--list',
			'--reporter=json'
		],
		{
			cwd: uiRoot,
			encoding: 'utf8',
			timeout: 15_000,
			maxBuffer: 1024 * 1024,
			// Playwright's standalone CLI must not inherit Bun's Jest worker marker.
			env: { ...process.env, JEST_WORKER_ID: undefined }
		}
	);
	expect(result.error).toBeUndefined();
	const report = JSON.parse(result.stdout) as { errors: unknown[]; suites: ListedSuite[] };
	expect(report.errors).toEqual([]);
	expect(result.status, result.stderr).toBe(0);
	expect(report.suites.map((suite) => suite.file)).toEqual(['timeline.performance.ts']);
}, 20_000);
