import { createHash, randomInt } from 'node:crypto';
import { spawn, type ChildProcess } from 'node:child_process';
import { createReadStream, existsSync } from 'node:fs';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
	nineCameraCircularStartSeparationSeconds,
	nineCameraKeyframeIntervalsSeconds,
	nineCameraKeyframeIntervalSeconds,
	nineCameraMinimumStartSeparationSeconds,
	nineCameraProfiles,
	nineCameraProfileVariants,
	type NineCameraKeyframeIntervalSeconds,
	type NineCameraProfile
} from '../src/lib/server/storybook/nine-camera-fixture';

const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const testRoot = path.join(repositoryRoot, 'target', 'nine-camera-demo');
const storageRoot = path.join(testRoot, 'recordings');
const configPath = path.join(testRoot, 'config.toml');
const draftsPath = path.join(testRoot, 'camera-drafts.json');
const fixtureManifestPath = path.join(
	repositoryRoot,
	'target',
	'demo-fixtures',
	'big-buck-bunny-camera-profiles.json'
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
	durationSeconds: number;
	variants: FixtureVariant[];
	blackIntervals: BlackInterval[];
};

type FixtureVariant = {
	keyframeIntervalSeconds: NineCameraKeyframeIntervalSeconds;
	profiles: FixtureProfile[];
};

type FixtureProfile = NineCameraProfile & {
	outputFile: string;
	outputSha256: string;
};

type BlackInterval = {
	startSeconds: number;
	endSeconds: number;
};

type CameraStart = {
	id: string;
	name: string;
	startAtSeconds: number;
	keyframeIntervalSeconds: NineCameraKeyframeIntervalSeconds;
	profilePair: readonly NineCameraProfile[];
};

type CameraDraft = Omit<CameraStart, 'profilePair'> & {
	profiles: NineCameraProfile[];
	ip: string;
	displayName: string;
	username: string;
	password: string;
	mainRtspUrl: string;
	subRtspUrl: string;
	backend: string;
	transport: string;
};

type TestCamera = {
	process: ChildProcess;
	draft: CameraDraft;
	config: string;
};

type CameraConfigEntry = {
	ip: string;
	username: string;
	password: string;
	main_rtsp_url: string;
	sub_rtsp_url: string;
	backend: string;
	transport: string;
};

type BunRuntime = typeof globalThis & {
	Bun: { TOML: { parse(input: string): unknown } };
};

const fixture = JSON.parse(await readFile(fixtureManifestPath, 'utf8')) as FixtureManifest;
if (!Array.isArray(fixture.variants)) {
	throw new Error(
		'Big Buck Bunny fixture manifest is outdated. Run `bun run demo:fixtures:prepare`.'
	);
}
for (const keyframeIntervalSeconds of nineCameraKeyframeIntervalsSeconds) {
	const variant = fixture.variants.find(
		(candidate) => candidate.keyframeIntervalSeconds === keyframeIntervalSeconds
	);
	if (!variant || !Array.isArray(variant.profiles)) {
		throw new Error(
			`Missing ${keyframeIntervalSeconds}s Big Buck Bunny fixture. Run \`bun run demo:fixtures:prepare\` first.`
		);
	}
	for (const profile of nineCameraProfileVariants) {
		const generated = fixtureProfile(variant, profile.stream, profile.codec);
		if (
			generated.codec !== profile.codec ||
			generated.width !== profile.width ||
			generated.height !== profile.height ||
			generated.framesPerSecond !== profile.framesPerSecond ||
			generated.bitrateKbps !== profile.bitrateKbps ||
			!existsSync(generated.outputFile) ||
			(await sha256(generated.outputFile)) !== generated.outputSha256
		) {
			throw new Error(
				`Invalid ${keyframeIntervalSeconds}s Big Buck Bunny ${profile.stream} fixture. Run \`bun run demo:fixtures:prepare --force\` first.`
			);
		}
	}
}

await rm(testRoot, { recursive: true, force: true });
await mkdir(storageRoot, { recursive: true });

const safeBeforeSeconds = 1;
const safeAfterSeconds = 65;
const cameraStarts = randomCameraStarts(fixture);
const testCameras: TestCamera[] = [];
try {
	for (const camera of cameraStarts) {
		testCameras.push(await startTestCamera(camera, fixtureVariant(fixture, camera)));
	}
} catch (error) {
	await stopCameras(testCameras);
	throw error;
}

await writeFile(
	draftsPath,
	`${JSON.stringify(
		{
			schemaVersion: 1,
			fixtureSha256: fixtureSetSha256(fixture.variants),
			selection: {
				sourceDurationSeconds: fixture.durationSeconds,
				minimumStartSeparationSeconds: nineCameraMinimumStartSeparationSeconds,
				safeBeforeSeconds,
				safeAfterSeconds,
				excludedBlackIntervals: fixture.blackIntervals
			},
			cameras: testCameras.map((camera) => camera.draft)
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
	env: { ...process.env, RUST_LOG: process.env.RUST_LOG ?? 'info,keeppeek=debug' },
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

async function startTestCamera(camera: CameraStart, variant: FixtureVariant): Promise<TestCamera> {
	const selectedMain = camera.profilePair.find((profile) => profile.stream === 'main')!;
	const selectedSub = camera.profilePair.find((profile) => profile.stream === 'sub')!;
	const mainProfile = fixtureProfile(variant, 'main', selectedMain.codec);
	const subProfile = fixtureProfile(variant, 'sub', selectedSub.codec);
	const cameraProcess = spawn(
		testCameraBinary,
		[
			'rtsp',
			'--main',
			mainProfile.outputFile,
			'--sub',
			subProfile.outputFile,
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
	const config = await new Promise<string>((resolveConfig, rejectConfig) => {
		let output = '';
		let configured = false;
		stdout.on('data', (chunk: string) => {
			if (configured) return;
			output += chunk;
			const configEnd = output.indexOf(transportLine);
			if (configEnd === -1) return;
			configured = true;
			resolveConfig(output.slice(0, configEnd + transportLine.length));
		});
		cameraProcess.once('error', rejectConfig);
		cameraProcess.once('exit', () => {
			if (!configured) {
				rejectConfig(
					new Error(`${camera.name} test camera exited before printing its configuration`)
				);
			}
		});
	});
	const parsed = (globalThis as BunRuntime).Bun.TOML.parse(config) as {
		'test-camera'?: Record<string, CameraConfigEntry>;
	};
	const entry = parsed['test-camera']?.[camera.name];
	if (!entry) {
		cameraProcess.kill('SIGINT');
		throw new Error(`Unable to parse ${camera.name} test camera configuration`);
	}
	const { profilePair, ...cameraDraft } = camera;
	return {
		process: cameraProcess,
		config,
		draft: {
			...cameraDraft,
			profiles: [...profilePair],
			ip: entry.ip,
			displayName: camera.name,
			username: entry.username,
			password: entry.password,
			mainRtspUrl: entry.main_rtsp_url,
			subRtspUrl: entry.sub_rtsp_url,
			backend: entry.backend,
			transport: entry.transport
		}
	};
}

function randomCameraStarts(fixture: FixtureManifest): CameraStart[] {
	const availableSeconds = fixture.durationSeconds;
	if (availableSeconds <= cameraNames.length * nineCameraMinimumStartSeparationSeconds) {
		throw new Error(`Big Buck Bunny fixture is too short: ${fixture.durationSeconds}s`);
	}
	const durationMilliseconds = Math.floor(availableSeconds * 1_000);
	for (let setAttempt = 0; setAttempt < 100; setAttempt += 1) {
		const starts: number[] = [];
		for (let cameraIndex = 0; cameraIndex < cameraNames.length; cameraIndex += 1) {
			let selected = false;
			for (let candidateAttempt = 0; candidateAttempt < 1_000; candidateAttempt += 1) {
				const candidate = randomInt(durationMilliseconds) / 1_000;
				const overlapsBlackInterval = fixture.blackIntervals.some(
					(interval) =>
						candidate - safeBeforeSeconds < interval.endSeconds &&
						candidate + safeAfterSeconds > interval.startSeconds
				);
				const tooClose =
					starts.length > 0 &&
					nineCameraCircularStartSeparationSeconds([...starts, candidate], availableSeconds) <
						nineCameraMinimumStartSeparationSeconds;
				if (overlapsBlackInterval || tooClose) continue;
				starts.push(candidate);
				selected = true;
				break;
			}
			if (!selected) break;
		}
		if (starts.length !== cameraNames.length) continue;
		return cameraNames.map((name, index) => ({
			id: `192.0.2.${101 + index}`,
			name,
			startAtSeconds: starts[index]!,
			keyframeIntervalSeconds: nineCameraKeyframeIntervalSeconds(index),
			profilePair: nineCameraProfiles(index)
		}));
	}
	throw new Error('Unable to select separated nonblack starts for all nine cameras');
}

function fixtureVariant(fixture: FixtureManifest, camera: CameraStart): FixtureVariant {
	const variant = fixture.variants.find(
		(candidate) => candidate.keyframeIntervalSeconds === camera.keyframeIntervalSeconds
	);
	if (!variant) {
		throw new Error(`Big Buck Bunny fixture has no ${camera.keyframeIntervalSeconds}s variant`);
	}
	return variant;
}

function fixtureProfile(
	variant: FixtureVariant,
	stream: 'main' | 'sub',
	codec: 'h264' | 'h265'
): FixtureProfile {
	const profile = variant.profiles.find(
		(candidate) => candidate.stream === stream && candidate.codec === codec
	);
	if (!profile) {
		throw new Error(
			`Big Buck Bunny ${variant.keyframeIntervalSeconds}s fixture has no ${stream} ${codec} profile`
		);
	}
	return profile;
}

function fixtureSetSha256(variants: readonly FixtureVariant[]): string {
	const hash = createHash('sha256');
	for (const variant of variants.toSorted(
		(left, right) => left.keyframeIntervalSeconds - right.keyframeIntervalSeconds
	)) {
		for (const profile of variant.profiles.toSorted((left, right) =>
			`${left.stream}:${left.codec}`.localeCompare(`${right.stream}:${right.codec}`)
		)) {
			hash.update(`${variant.keyframeIntervalSeconds}:${profile.stream}:${profile.outputSha256}\n`);
		}
	}
	return hash.digest('hex');
}

async function sha256(filePath: string): Promise<string> {
	const hash = createHash('sha256');
	for await (const chunk of createReadStream(filePath)) hash.update(chunk);
	return hash.digest('hex');
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
