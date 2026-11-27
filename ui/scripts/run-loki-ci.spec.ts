import { describe, expect, test } from 'bun:test';
import { mkdir, mkdtemp, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import {
	buildLokiTestArgs,
	hasVisualDifferences,
	LOKI_CHROME_CONCURRENCY,
	LOKI_CHROME_LOAD_TIMEOUT_MS,
	LOKI_CHROME_RETRIES,
	MAX_LOKI_HARNESS_ATTEMPTS,
	runWithHarnessRetry
} from '../visual-harness/run-loki-ci';

describe('buildLokiTestArgs', () => {
	test('includes bounded Chrome concurrency, retry, and timeout flags', () => {
		const args = buildLokiTestArgs('desktop', 'http://host.docker.internal:6006');

		expect(args).toEqual([
			'test',
			'--reactUri',
			'http://host.docker.internal:6006',
			'--configurationFilter',
			'^chrome\\.desktop$',
			'--output',
			'.loki/current/desktop',
			'--difference',
			'.loki/difference/desktop',
			'--requireReference=false',
			'--verboseRenderer',
			'--chromeConcurrency',
			String(LOKI_CHROME_CONCURRENCY),
			'--chromeRetries',
			String(LOKI_CHROME_RETRIES),
			'--chromeLoadTimeout',
			String(LOKI_CHROME_LOAD_TIMEOUT_MS)
		]);
	});
});

describe('hasVisualDifferences', () => {
	test('returns false when difference directory does not exist, is empty, or has only non-png files', async () => {
		const tempRoot = await mkdtemp(join(tmpdir(), 'loki-diff-check-'));
		try {
			const missingDir = join(tempRoot, 'missing');
			expect(await hasVisualDifferences(missingDir)).toBe(false);

			const emptyDir = join(tempRoot, 'empty');
			await mkdir(emptyDir, { recursive: true });
			expect(await hasVisualDifferences(emptyDir)).toBe(false);

			await writeFile(join(emptyDir, 'notes.txt'), 'not an image');
			expect(await hasVisualDifferences(emptyDir)).toBe(false);
		} finally {
			await rm(tempRoot, { recursive: true, force: true });
		}
	});

	test('returns true when a nested png difference file exists', async () => {
		const tempRoot = await mkdtemp(join(tmpdir(), 'loki-diff-nested-'));
		try {
			const nestedConfigDir = join(tempRoot, 'chrome.desktop');
			await mkdir(nestedConfigDir, { recursive: true });
			await writeFile(join(nestedConfigDir, 'peek.desktop.live-wall.png'), 'png');

			expect(await hasVisualDifferences(tempRoot)).toBe(true);
		} finally {
			await rm(tempRoot, { recursive: true, force: true });
		}
	});
});

describe('runWithHarnessRetry', () => {
	test('retries once on a non-diff crash, clears stale current output, and succeeds on attempt 2', async () => {
		const tempRoot = await mkdtemp(join(tmpdir(), 'loki-retry-success-'));
		try {
			const differenceDir = join(tempRoot, 'difference', 'desktop');
			const currentDir = join(tempRoot, 'current', 'desktop');
			const logs: string[] = [];
			let attempts = 0;

			const exitCode = await runWithHarnessRetry({
				configuration: 'desktop',
				differenceDir,
				currentDir,
				log: (message) => logs.push(message),
				executeAttempt: async () => {
					attempts += 1;
					if (attempts === 1) {
						await mkdir(currentDir, { recursive: true });
						await writeFile(join(currentDir, 'partial.png'), 'stale');
						return 1;
					}
					await expect(stat(currentDir)).rejects.toThrow();
					return 0;
				}
			});

			expect(exitCode).toBe(0);
			expect(attempts).toBe(2);
			expect(logs).toEqual([
				`Loki desktop failed with exit code 1 and no visual diffs; retrying harness attempt 2/${MAX_LOKI_HARNESS_ATTEMPTS}.`
			]);
		} finally {
			await rm(tempRoot, { recursive: true, force: true });
		}
	});

	test('fails immediately without retrying when a visual diff png exists', async () => {
		const tempRoot = await mkdtemp(join(tmpdir(), 'loki-retry-diff-'));
		try {
			const differenceDir = join(tempRoot, 'difference', 'desktop');
			const currentDir = join(tempRoot, 'current', 'desktop');
			const logs: string[] = [];
			let attempts = 0;

			const exitCode = await runWithHarnessRetry({
				configuration: 'desktop',
				differenceDir,
				currentDir,
				log: (message) => logs.push(message),
				executeAttempt: async () => {
					attempts += 1;
					const nestedDir = join(differenceDir, 'chrome.desktop');
					await mkdir(nestedDir, { recursive: true });
					await writeFile(join(nestedDir, 'peek.desktop.live-wall.png'), 'diff');
					return 1;
				}
			});

			expect(exitCode).toBe(1);
			expect(attempts).toBe(1);
			expect(logs).toEqual([]);
		} finally {
			await rm(tempRoot, { recursive: true, force: true });
		}
	});

	test('stops after maxAttempts when non-diff failures repeat', async () => {
		const tempRoot = await mkdtemp(join(tmpdir(), 'loki-retry-bounded-'));
		try {
			const differenceDir = join(tempRoot, 'difference', 'mobile');
			const currentDir = join(tempRoot, 'current', 'mobile');
			const logs: string[] = [];
			let attempts = 0;

			const exitCode = await runWithHarnessRetry({
				configuration: 'mobile',
				differenceDir,
				currentDir,
				log: (message) => logs.push(message),
				executeAttempt: async () => {
					attempts += 1;
					return attempts === 1 ? 2 : 3;
				}
			});

			expect(exitCode).toBe(3);
			expect(attempts).toBe(MAX_LOKI_HARNESS_ATTEMPTS);
			expect(logs).toHaveLength(1);
		} finally {
			await rm(tempRoot, { recursive: true, force: true });
		}
	});

	test('rejects invalid maxAttempts values', async () => {
		for (const invalidAttempts of [0, -1, 1.5]) {
			await expect(
				runWithHarnessRetry({
					configuration: 'desktop',
					differenceDir: 'unused-diff',
					currentDir: 'unused-current',
					maxAttempts: invalidAttempts,
					executeAttempt: async () => 0
				})
			).rejects.toThrow(RangeError);
		}
	});
});
