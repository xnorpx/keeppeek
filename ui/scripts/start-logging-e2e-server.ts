import { mkdir, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';

const repositoryRoot = path.resolve(import.meta.dir, '../..');
const testRoot = path.join(repositoryRoot, 'target', 'ui-logging-e2e');
const storageRoot = path.join(testRoot, 'recordings');
const configPath = path.join(testRoot, 'config.toml');
const testdataRoot = path.join(repositoryRoot, 'crates', 'test-camera', 'testdata');

type TestCamera = {
	name: string;
	process: ReturnType<typeof Bun.spawn>;
	config: string;
};

async function startTestCamera(name: string, main: string, sub: string): Promise<TestCamera> {
	const camera = Bun.spawn(
		[
			'cargo',
			'run',
			'--quiet',
			'-p',
			'test-camera',
			'--bin',
			'test_camera',
			'--',
			'rtsp',
			'--main',
			main,
			'--sub',
			sub,
			'--name',
			name
		],
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
			path.join(testdataRoot, 'cc-4k-3840x2160-h264.mp4'),
			path.join(testdataRoot, 'cc-4k-640x360-h264.mp4')
		)
	);
} catch (error) {
	for (const camera of testCameras) camera.process.kill('SIGINT');
	await Promise.all(testCameras.map((camera) => camera.process.exited));
	throw error;
}

const tomlString = (value: string) => JSON.stringify(value);
await writeFile(
	configPath,
	`host = "127.0.0.1"
port = 4317

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

const server = Bun.spawn(
	[
		'cargo',
		'run',
		'--quiet',
		'-p',
		'keeppeek',
		'--bin',
		'keeppeek',
		'--',
		`--config=${configPath}`
	],
	{
		cwd: repositoryRoot,
		env: { ...process.env, RUST_LOG: 'info,keeppeek=debug' },
		stdout: 'inherit',
		stderr: 'inherit'
	}
);

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
