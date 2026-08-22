import { randomInt } from 'node:crypto';
import { spawn, type ChildProcess } from 'node:child_process';
import { existsSync } from 'node:fs';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const testRoot = path.join(repositoryRoot, 'target', 'nine-camera-demo');
const storageRoot = path.join(testRoot, 'recordings');
const configPath = path.join(testRoot, 'config.toml');
const startsPath = path.join(testRoot, 'camera-starts.json');
const fixtureManifestPath = path.join(
	repositoryRoot,
	'target',
	'demo-fixtures',
	'big-buck-bunny-640x360-h264.json'
);
const executableExtension = process.platform === 'win32' ? '.exe' : '';
const releaseRoot = path.join(repositoryRoot, 'target', 'release');
const keeppeekBinary = requiredBinary('keeppeek');
const testCameraBinary = requiredBinary('test_camera');
const cameraNames = [
	'North Meadow',
	'Forest Edge',
	'Rabbit Burrow',
	'Creek Bank',
	'South Hill',
	'Pine Ridge',
	'Old Orchard',
	'River Bend',
	'West Field'
];

type FixtureManifest = {
	outputFile: string;
	durationSeconds: number;
	outputSha256: string;
	blackIntervals: BlackInterval[];
};

type BlackInterval = {
	startSeconds: number;
	endSeconds: number;
};

type CameraStart = {
	id: string;
	name: string;
	startAtSeconds: number;
};

type TestCamera = CameraStart & {
	process: ChildProcess;
	config: string;
};

const fixture = JSON.parse(await readFile(fixtureManifestPath, 'utf8')) as FixtureManifest;
if (!existsSync(fixture.outputFile)) {
	throw new Error('Missing Big Buck Bunny fixture. Run `bun run demo:fixtures:prepare` first.');
}

await rm(testRoot, { recursive: true, force: true });
await mkdir(storageRoot, { recursive: true });

const safeBeforeSeconds = 1;
const safeAfterSeconds = 30;
const cameraStarts = randomCameraStarts(fixture);
const testCameras: TestCamera[] = [];
try {
	for (const camera of cameraStarts) {
		testCameras.push(await startTestCamera(camera, fixture.outputFile));
	}
} catch (error) {
	await stopCameras(testCameras);
	throw error;
}

await writeFile(
	startsPath,
	`${JSON.stringify(
		{
			schemaVersion: 1,
			fixtureSha256: fixture.outputSha256,
			selection: {
				safeBeforeSeconds,
				safeAfterSeconds,
				excludedBlackIntervals: fixture.blackIntervals
			},
			cameras: cameraStarts
		},
		null,
		2
	)}\n`
);

const tomlString = (value: string) => JSON.stringify(value);
await writeFile(
	configPath,
	`host = "127.0.0.1"
port = 4318

[direct_card]
allowed_origins = ["http://127.0.0.1:4175"]

[storage]
medium_term_path = ${tomlString(storageRoot)}
long_term_path = ${tomlString(storageRoot)}
recording_catalog_path = ${tomlString(path.join(testRoot, 'recordings.db'))}
event_thumbnail_path = ${tomlString(path.join(testRoot, 'event-thumbnails'))}
event_thumbnail_max_mb = 16
short_term_secs = 5
medium_term_secs = 60
flush_interval_secs = 1
write_buffer_bytes = 8192
long_term_max_gb = 0

${testCameras.map((camera) => camera.config).join('\n')}
`
);

const server = spawn(keeppeekBinary, [`--config=${configPath}`], {
	cwd: repositoryRoot,
	env: { ...process.env, RUST_LOG: 'info,keeppeek=debug' },
	stdio: 'inherit'
});

let stopping = false;
async function stopServer(): Promise<void> {
	if (stopping) return;
	stopping = true;
	server.kill('SIGINT');
	await stopCameras(testCameras);
}

process.once('SIGINT', () => void stopServer());
process.once('SIGTERM', () => void stopServer());
process.once('exit', () => {
	for (const camera of testCameras) camera.process.kill('SIGINT');
});

const exitCode = await new Promise<number | null>((resolveExit, rejectExit) => {
	server.once('error', rejectExit);
	server.once('exit', (code) => resolveExit(code));
});
await stopCameras(testCameras);
process.exitCode = stopping ? 0 : (exitCode ?? 1);

async function startTestCamera(camera: CameraStart, mediaPath: string): Promise<TestCamera> {
	const cameraProcess = spawn(
		testCameraBinary,
		[
			'rtsp',
			'--main',
			mediaPath,
			'--sub',
			mediaPath,
			'--name',
			camera.name,
			'--config-ip',
			camera.id,
			'--start-at-seconds',
			camera.startAtSeconds.toFixed(3)
		],
		{ cwd: repositoryRoot, stdio: ['ignore', 'pipe', 'inherit'] }
	);
	const stdout = cameraProcess.stdout;
	if (!stdout) {
		cameraProcess.kill('SIGINT');
		throw new Error(`Unable to capture ${camera.name} test camera configuration`);
	}

	stdout.setEncoding('utf8');
	const transportLine = 'transport = "tcp"\n';
	let output = '';
	for await (const chunk of stdout) {
		output += chunk;
		const configEnd = output.indexOf(transportLine);
		if (configEnd !== -1) {
			return {
				...camera,
				process: cameraProcess,
				config: output.slice(0, configEnd + transportLine.length)
			};
		}
	}

	throw new Error(`${camera.name} test camera exited before printing its configuration`);
}

function randomCameraStarts(fixture: FixtureManifest): CameraStart[] {
	const minimumRemainingSeconds = 90;
	const availableSeconds = fixture.durationSeconds - minimumRemainingSeconds;
	if (availableSeconds < cameraNames.length) {
		throw new Error(`Big Buck Bunny fixture is too short: ${fixture.durationSeconds}s`);
	}
	const bandSeconds = availableSeconds / cameraNames.length;
	return cameraNames.map((name, index) => {
		const bandMilliseconds = Math.max(1, Math.floor(bandSeconds * 1_000));
		let startAtSeconds: number | undefined;
		for (let attempt = 0; attempt < 1_000; attempt += 1) {
			const candidate = (index * bandSeconds * 1_000 + randomInt(bandMilliseconds)) / 1_000;
			if (
				!fixture.blackIntervals.some(
					(interval) =>
						candidate - safeBeforeSeconds < interval.endSeconds &&
						candidate + safeAfterSeconds > interval.startSeconds
				)
			) {
				startAtSeconds = candidate;
				break;
			}
		}
		if (startAtSeconds === undefined) {
			throw new Error(`Unable to find a nonblack start for ${name}`);
		}
		return {
			id: `192.0.2.${101 + index}`,
			name,
			startAtSeconds: Number(startAtSeconds.toFixed(3))
		};
	});
}

async function stopCameras(cameras: TestCamera[]): Promise<void> {
	for (const camera of cameras) camera.process.kill('SIGINT');
	await Promise.all(
		cameras.map(
			(camera) =>
				new Promise<void>((resolveExit) => {
					if (camera.process.exitCode !== null) resolveExit();
					else camera.process.once('exit', () => resolveExit());
				})
		)
	);
}

function requiredBinary(binaryName: string): string {
	const binaryPath = path.join(releaseRoot, `${binaryName}${executableExtension}`);
	if (!existsSync(binaryPath)) {
		throw new Error(
			`Missing release E2E binary: ${binaryPath}. Run \`bun run test:e2e:prepare\` first.`
		);
	}
	return binaryPath;
}
