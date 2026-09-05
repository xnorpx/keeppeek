import { existsSync } from 'node:fs';
import { mkdir, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';

const repositoryRoot = path.resolve(import.meta.dir, '../..');
const testRoot = path.join(repositoryRoot, 'target', 'ui-logging-e2e');
const storageRoot = path.join(testRoot, 'recordings');
const configPath = path.join(testRoot, 'config.toml');
const cameraDraftPath = path.join(testRoot, 'camera-draft.json');
const testdataRoot = path.join(repositoryRoot, 'crates', 'test-camera', 'testdata');
const executableExtension = process.platform === 'win32' ? '.exe' : '';
const releaseRoot = path.join(repositoryRoot, 'target', 'release');
const keeppeekBinary = requiredBinary('keeppeek');
const testCameraBinary = requiredBinary('test_camera');
const startWithEmptyFleet = process.env.KEEPPEEK_E2E_EMPTY_FLEET === '1';
const backendPort = Number(process.env.KEEPPEEK_E2E_BACKEND_PORT ?? '4317');
const frontendPort = Number(process.env.KEEPPEEK_E2E_FRONTEND_PORT ?? '4174');
if (!Number.isInteger(backendPort) || backendPort < 1 || backendPort > 65_535) {
	throw new Error('KEEPPEEK_E2E_BACKEND_PORT must be an integer from 1 through 65535');
}
if (!Number.isInteger(frontendPort) || frontendPort < 1 || frontendPort > 65_535) {
	throw new Error('KEEPPEEK_E2E_FRONTEND_PORT must be an integer from 1 through 65535');
}

type TestCamera = {
	name: string;
	process: ReturnType<typeof Bun.spawn>;
	config: string;
};

type TestCameraDraft = {
	ip: string;
	displayName: string;
	username: string;
	password: string;
	onvifPort: string;
	httpPort: string;
	mainRtspUrl: string;
	subRtspUrl: string;
	backend: string;
	transport: string;
};

type ParsedTestCameraConfig = {
	'test-camera': Record<
		string,
		{
			ip: string;
			username: string;
			password: string;
			onvif_port: number;
			http_port: number;
			main_rtsp_url: string;
			sub_rtsp_url: string;
			backend: string;
			transport: string;
		}
	>;
};

async function startTestCamera(name: string, main: string, sub: string): Promise<TestCamera> {
	const camera = Bun.spawn(
		[testCameraBinary, 'rtsp', '--main', main, '--sub', sub, '--name', name],
		{
			cwd: repositoryRoot,
			stdout: 'pipe',
			stderr: 'inherit'
		}
	);
	const stdout = camera.stdout;
	if (!stdout) {
		camera.kill('SIGINT');
		throw new Error(`Unable to capture ${name} test camera configuration`);
	}

	const reader = stdout.getReader();
	const decoder = new TextDecoder();
	const transportLine = 'transport = "tcp"\n';
	let output = '';
	while (true) {
		const { done, value } = await reader.read();
		if (value) output += decoder.decode(value, { stream: true });
		const configEnd = output.indexOf(transportLine);
		if (configEnd !== -1) {
			reader.releaseLock();
			return {
				name,
				process: camera,
				config: output.slice(0, configEnd + transportLine.length)
			};
		}
		if (done) break;
	}

	throw new Error(`${name} test camera exited before printing its configuration`);
}

await rm(testRoot, { recursive: true, force: true });
await mkdir(storageRoot, { recursive: true });

const testCameras: TestCamera[] = [];
try {
	testCameras.push(
		await startTestCamera(
			'e2e-h264',
			path.join(testdataRoot, 'cc-4k-640x360-h264.mp4'),
			path.join(testdataRoot, 'cc-4k-640x360-h264.mp4')
		)
	);
} catch (error) {
	for (const camera of testCameras) camera.process.kill('SIGINT');
	await Promise.all(testCameras.map((camera) => camera.process.exited));
	throw error;
}

const testCamera = testCameras[0];
if (!testCamera) throw new Error('The logging E2E server requires a test camera');
await writeFile(cameraDraftPath, `${JSON.stringify(parseCameraDraft(testCamera), null, 2)}\n`);

const tomlString = (value: string) => JSON.stringify(value);
await writeFile(
	configPath,
	`host = "127.0.0.1"
port = ${backendPort}

[access]
require_secure_remote = false

[direct_card]
allowed_origins = ["http://127.0.0.1:${frontendPort}"]

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

${startWithEmptyFleet ? '' : testCameras.map((camera) => camera.config).join('\n')}
`
);

const seed = Bun.spawn(
	[
		testCameraBinary,
		'seed-recording',
		'--source',
		path.join(testdataRoot, 'cc-4k-640x360-h264.mp4'),
		'--recordings',
		storageRoot,
		'--catalog',
		path.join(testRoot, 'recordings.db'),
		'--stream-id',
		'e2e-h264/main'
	],
	{ cwd: repositoryRoot, stdout: 'inherit', stderr: 'inherit' }
);
const seedExitCode = await seed.exited;
if (seedExitCode !== 0) {
	for (const camera of testCameras) camera.process.kill('SIGINT');
	await Promise.all(testCameras.map((camera) => camera.process.exited));
	throw new Error(`Recording seed exited with code ${seedExitCode}`);
}

const server = Bun.spawn([keeppeekBinary, `--config=${configPath}`], {
	cwd: repositoryRoot,
	env: { ...process.env, RUST_LOG: 'info,keeppeek=debug' },
	stdout: 'inherit',
	stderr: 'inherit'
});

let stopping = false;
function stopServer(): void {
	if (stopping) return;
	stopping = true;
	server.kill('SIGINT');
	for (const camera of testCameras) camera.process.kill('SIGINT');
}

process.once('SIGINT', stopServer);
process.once('SIGTERM', stopServer);
process.once('exit', stopServer);

const exitCode = await server.exited;
process.exitCode = stopping ? 0 : exitCode;

function parseCameraDraft(camera: TestCamera): TestCameraDraft {
	const parsed = Bun.TOML.parse(camera.config) as ParsedTestCameraConfig;
	const settings = parsed['test-camera'][camera.name];
	if (!settings) throw new Error(`Missing generated configuration for ${camera.name}`);
	return {
		ip: settings.ip,
		displayName: camera.name,
		username: settings.username,
		password: settings.password,
		onvifPort: settings.onvif_port.toString(),
		httpPort: settings.http_port.toString(),
		mainRtspUrl: settings.main_rtsp_url,
		subRtspUrl: settings.sub_rtsp_url,
		backend: settings.backend,
		transport: settings.transport
	};
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
