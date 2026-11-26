import { readdir, rm } from 'node:fs/promises';
import { resolve, sep } from 'node:path';

export type LokiConfiguration = 'desktop' | 'mobile';

export const LOKI_CHROME_CONCURRENCY = 2;
export const LOKI_CHROME_RETRIES = 2;
export const LOKI_CHROME_LOAD_TIMEOUT_MS = 60_000;
export const MAX_LOKI_HARNESS_ATTEMPTS = 2;

export type LokiHarnessRetryOptions = {
	configuration: LokiConfiguration;
	differenceDir: string;
	currentDir: string;
	maxAttempts?: number;
	executeAttempt: () => Promise<number>;
	log?: (message: string) => void;
};

export function buildLokiTestArgs(configuration: LokiConfiguration, reactUri: string): string[] {
	return [
		'test',
		'--reactUri',
		reactUri,
		'--configurationFilter',
		`^chrome\\.${configuration}$`,
		'--output',
		`.loki/current/${configuration}`,
		'--difference',
		`.loki/difference/${configuration}`,
		'--requireReference=false',
		'--verboseRenderer',
		'--chromeConcurrency',
		String(LOKI_CHROME_CONCURRENCY),
		'--chromeRetries',
		String(LOKI_CHROME_RETRIES),
		'--chromeLoadTimeout',
		String(LOKI_CHROME_LOAD_TIMEOUT_MS)
	];
}

export async function hasVisualDifferences(differenceDir: string): Promise<boolean> {
	let entries: string[];
	try {
		entries = await readdir(differenceDir, { recursive: true });
	} catch (error) {
		if (error instanceof Error && 'code' in error && error.code === 'ENOENT') {
			return false;
		}
		throw error;
	}
	return entries.some((entry) => entry.toLowerCase().endsWith('.png'));
}

export async function runWithHarnessRetry(options: LokiHarnessRetryOptions): Promise<number> {
	const maxAttempts = options.maxAttempts ?? MAX_LOKI_HARNESS_ATTEMPTS;
	if (!Number.isInteger(maxAttempts) || maxAttempts < 1) {
		throw new RangeError('maxAttempts must be a positive integer.');
	}

	// ponytail: Retry one non-diff Chrome or Docker failure; remove once Loki fixes port collisions and timeout crashes.
	let lastExitCode = 1;
	for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
		await rm(options.differenceDir, { recursive: true, force: true });
		if (attempt > 1) {
			await rm(options.currentDir, { recursive: true, force: true });
		}
		lastExitCode = await options.executeAttempt();
		if (lastExitCode === 0) return 0;
		if (await hasVisualDifferences(options.differenceDir)) return lastExitCode;
		if (attempt >= maxAttempts) return lastExitCode;
		(options.log ?? console.error)(
			`Loki ${options.configuration} failed with exit code ${lastExitCode} and no visual diffs; retrying harness attempt ${attempt + 1}/${maxAttempts}.`
		);
	}
	return lastExitCode;
}

function startStorybookServer(storybookRoot: string) {
	return Bun.serve({
		hostname: '0.0.0.0',
		port: 0,
		async fetch(request) {
			let pathname: string;
			try {
				pathname = decodeURIComponent(new URL(request.url).pathname);
			} catch {
				return new Response('Invalid path', { status: 400 });
			}
			const relativePath = pathname.replace(/^\/+/, '') || 'index.html';
			const filePath = resolve(storybookRoot, relativePath);
			if (filePath !== storybookRoot && !filePath.startsWith(`${storybookRoot}${sep}`)) {
				return new Response('Invalid path', { status: 400 });
			}
			const file = Bun.file(filePath);
			return (await file.exists())
				? new Response(file)
				: new Response('Not found', { status: 404 });
		}
	});
}

async function main(): Promise<void> {
	const harnessRoot = import.meta.dir;
	const storybookRoot = resolve(harnessRoot, 'storybook-static');
	if (!(await Bun.file(resolve(storybookRoot, 'index.html')).exists())) {
		throw new Error('Build Storybook before running Loki.');
	}

	const server = startStorybookServer(storybookRoot);
	const environment = { ...process.env };
	delete environment.CI;
	delete environment.CONTINUOUS_INTEGRATION;
	delete environment.BUILD_NUMBER;
	delete environment.RUN_ID;
	const lokiBinary = resolve(
		harnessRoot,
		'node_modules',
		'.bin',
		process.platform === 'win32' ? 'loki.cmd' : 'loki'
	);

	async function runLoki(configuration: LokiConfiguration): Promise<number> {
		return runWithHarnessRetry({
			configuration,
			differenceDir: resolve(harnessRoot, '.loki', 'difference', configuration),
			currentDir: resolve(harnessRoot, '.loki', 'current', configuration),
			executeAttempt: () => {
				const lokiProcess = Bun.spawn(
					[
						lokiBinary,
						...buildLokiTestArgs(configuration, `http://host.docker.internal:${server.port}`)
					],
					{
						cwd: harnessRoot,
						env: environment,
						stdin: 'inherit',
						stdout: 'inherit',
						stderr: 'inherit'
					}
				);
				return lokiProcess.exited;
			}
		});
	}

	try {
		const desktopExitCode = await runLoki('desktop');
		const mobileExitCode = await runLoki('mobile');
		if (desktopExitCode !== 0 || mobileExitCode !== 0) process.exitCode = 1;
	} finally {
		await server.stop(true);
	}
}

if (import.meta.main) {
	await main();
}
