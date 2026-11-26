import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync } from 'node:fs';
import { mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export type PlaybackCorpusCamera = {
	id: string;
	label: string;
	displayName: string;
	zones: string[];
};

export type PlaybackCorpusSampleEvent = {
	id: string;
	cameraId: string;
	cameraLabel: string;
	zone: string;
	text: string;
	startTimeMs: number;
	hasThumbnail: boolean;
};

export type PlaybackCorpusManifest = {
	date: string;
	dayStartMs: number;
	dayEndMs: number;
	cameras: PlaybackCorpusCamera[];
	recordingCount: number;
	fragmentCount: number;
	eventCount: number;
	bunnyEventCount: number;
	thumbnailCount: number;
	seedDurationMs: number;
	heroSegment: {
		cameraId: string;
		startMs: number;
		endMs: number;
	};
	scrubTargets: number[];
	sampleBunnyEvents: PlaybackCorpusSampleEvent[];
};

export type SeedPlaybackDatabaseOptions = {
	dir?: string;
	date?: string;
	port?: number;
	hours?: number;
	eventsPerCamera?: number;
};

export type SeededPlaybackDatabase = {
	rootDir: string;
	configPath: string;
	manifestPath: string;
	manifest: PlaybackCorpusManifest;
};

const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const releaseRoot = path.join(repositoryRoot, 'target', 'release');
const executableExtension = process.platform === 'win32' ? '.exe' : '';
const defaultSourcePath = path.join(
	repositoryRoot,
	'crates',
	'test-camera',
	'testdata',
	'cc-4k-640x360-h264.mp4'
);

export function requiredReleaseBinary(name: 'keeppeek' | 'test_camera'): string {
	const binaryPath = path.join(releaseRoot, `${name}${executableExtension}`);
	if (!existsSync(binaryPath)) {
		throw new Error(
			`Missing release binary: ${binaryPath}. Run \`bun run test:e2e:prepare\` from ui/ first.`
		);
	}
	return binaryPath;
}

export function defaultPlaybackCorpusDirectory(): string {
	if (process.platform === 'win32') {
		// Windows owned storage enforces NTFS ACLs; keep the temporary corpus on the system volume.
		const hash = createHash('sha256').update(repositoryRoot).digest('hex').slice(0, 12);
		return path.join(tmpdir(), `keeppeek-playback-corpus-${hash}`);
	}
	return path.join(repositoryRoot, 'target', 'playback-corpus');
}

export async function seedPlaybackDatabase(
	options: SeedPlaybackDatabaseOptions = {}
): Promise<SeededPlaybackDatabase> {
	const rootDir = path.resolve(options.dir ?? defaultPlaybackCorpusDirectory());
	const date = options.date ?? '2026-08-15';
	const port = options.port ?? 4399;
	const hours = options.hours ?? 24;
	const eventsPerCamera = options.eventsPerCamera ?? 180;

	if (!Number.isInteger(port) || port < 1 || port > 65_535) {
		throw new Error('port must be an integer between 1 and 65535');
	}

	const testCameraBinary = requiredReleaseBinary('test_camera');
	await rm(rootDir, { recursive: true, force: true });
	await mkdir(rootDir, { recursive: true });

	const configPath = path.join(rootDir, 'config.toml');
	const manifestPath = path.join(rootDir, 'manifest.json');
	await writeFile(
		configPath,
		`host = "127.0.0.1"
port = ${port}

[access]
require_secure_remote = false

[storage]
event_thumbnail_max_mb = 64
short_term_secs = 5
medium_term_secs = 60
flush_interval_secs = 1
write_buffer_bytes = 8192
long_term_max_gb = 0
minimum_free_gb = 0
maximum_used_percent = 99

[cameras.backyard-garden]
ip = "192.0.2.11"
display_name = "Backyard Garden"
username = "test"
password = "test"
recording_mode = "off"

[cameras.front-driveway]
ip = "192.0.2.12"
display_name = "Front Driveway"
username = "test"
password = "test"
recording_mode = "off"
`
	);

	const output = await runCommandCapture(
		testCameraBinary,
		[
			'seed-playback-corpus',
			'--source',
			defaultSourcePath,
			'--config',
			configPath,
			'--date',
			date,
			'--hours',
			String(hours),
			'--events-per-camera',
			String(eventsPerCamera)
		],
		repositoryRoot
	);
	const manifest = JSON.parse(output) as PlaybackCorpusManifest;
	await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
	return { rootDir, configPath, manifestPath, manifest };
}

function runCommandCapture(command: string, args: string[], cwd: string): Promise<string> {
	return new Promise((resolve, reject) => {
		const child = spawn(command, args, {
			cwd,
			env: { ...process.env, RUST_LOG: 'warn' },
			stdio: ['ignore', 'pipe', 'pipe'],
			windowsHide: true
		});
		let stdout = '';
		let stderr = '';
		child.stdout.setEncoding('utf8');
		child.stderr.setEncoding('utf8');
		child.stdout.on('data', (chunk: string) => {
			stdout += chunk;
		});
		child.stderr.on('data', (chunk: string) => {
			stderr += chunk;
		});
		child.on('error', reject);
		child.on('close', (code) => {
			if (code !== 0) {
				reject(new Error(`${path.basename(command)} exited with ${code}: ${stderr.trim()}`));
				return;
			}
			resolve(stdout);
		});
	});
}

function cliArgument(name: string): string | undefined {
	const index = process.argv.indexOf(`--${name}`);
	return index >= 0 ? process.argv[index + 1] : undefined;
}

if (import.meta.main) {
	const dir = cliArgument('dir');
	const date = cliArgument('date');
	const portValue = cliArgument('port');
	const hoursValue = cliArgument('hours');
	const eventsValue = cliArgument('events-per-camera');
	const serve = process.argv.includes('--serve');

	const seeded = await seedPlaybackDatabase({
		dir,
		date,
		port: portValue ? Number(portValue) : undefined,
		hours: hoursValue ? Number(hoursValue) : undefined,
		eventsPerCamera: eventsValue ? Number(eventsValue) : undefined
	});
	const { manifest } = seeded;
	console.log(
		`Seeded playback database in ${manifest.seedDurationMs} ms at ${seeded.rootDir}\n` +
			`  recordings: ${manifest.recordingCount}, fragments: ${manifest.fragmentCount}\n` +
			`  events: ${manifest.eventCount} (${manifest.bunnyEventCount} bunnies, ${manifest.thumbnailCount} thumbnails)\n` +
			`  config: ${seeded.configPath}\n` +
			`  manifest: ${seeded.manifestPath}`
	);

	if (serve) {
		const keeppeekBinary = requiredReleaseBinary('keeppeek');
		const port = portValue ? Number(portValue) : 4399;
		const hero = manifest.heroSegment;
		console.log(
			`Starting KeepPeek on http://127.0.0.1:${port}\n` +
				`  Keep timeline: http://127.0.0.1:${port}/keep?camera=${hero.cameraId}&stream=sub&date=${manifest.date}&at=${hero.startMs}\n` +
				`  Bunny events:  http://127.0.0.1:${port}/events?date=${manifest.date}&q=bunny`
		);
		const server = spawn(keeppeekBinary, [`--config=${seeded.configPath}`], {
			cwd: repositoryRoot,
			stdio: 'inherit'
		});
		const stop = () => server.kill('SIGINT');
		process.once('SIGINT', stop);
		process.once('SIGTERM', stop);
		await new Promise<void>((resolve) => {
			server.on('exit', () => resolve());
		});
	}
}
