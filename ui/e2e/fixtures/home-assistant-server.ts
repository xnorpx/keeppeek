import { spawn, type ChildProcess } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { access, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

export const cardTestKeys = [
	'550e8400-e29b-41d4-a716-446655440001',
	'550e8400-e29b-41d4-a716-446655440002',
	'550e8400-e29b-41d4-a716-446655440003'
] as const;
export const cardAdminKey = '550e8400-e29b-41d4-a716-446655440004';
export const cardSourceIds = ['192.0.2.101', '192.0.2.102'] as const;
const repositoryRoot = resolve(import.meta.dirname, '../../..');

async function availablePort(): Promise<number> {
	const reservation = createServer();
	reservation.listen(0, '127.0.0.1');
	await once(reservation, 'listening');
	const address = reservation.address();
	if (!address || typeof address === 'string') throw new Error('No fixture port was allocated.');
	await new Promise<void>((resolveClose, reject) =>
		reservation.close((error) => (error ? reject(error) : resolveClose()))
	);
	return address.port;
}

async function cameraConfig(child: ChildProcess): Promise<string> {
	return new Promise((resolveConfig, reject) => {
		let output = '';
		const timer = setTimeout(
			() => finish(new Error('Test camera configuration timed out.')),
			10_000
		);
		const onExit = () => finish(new Error('Test camera exited before initialization.'));
		const onData = (chunk: Buffer) => {
			output += chunk.toString();
			if (output.length > 16_384) {
				finish(new Error('Test camera configuration exceeded its bound.'));
				return;
			}
			const end = /transport = "tcp"\r?\n/.exec(output);
			if (end) finish(null, output.slice(0, end.index + end[0].length));
		};
		function finish(error: Error | null, config = '') {
			clearTimeout(timer);
			child.off('exit', onExit);
			child.stdout?.off('data', onData);
			child.stdout?.resume();
			if (error) reject(error);
			else resolveConfig(config);
		}
		child.once('exit', onExit);
		child.stdout?.on('data', onData);
	});
}

function credentialRecord(key: string, administrator: boolean): string {
	const verifier = [
		...createHash('sha256')
			.update(Buffer.from(key.replaceAll('-', ''), 'hex'))
			.digest()
	];
	return `[[access_credentials.credentials]]
id = ${JSON.stringify(key)}
name = ${JSON.stringify(administrator ? 'Card test administrator' : `Card test ${key.slice(-1)}`)}
role = ${JSON.stringify(administrator ? 'administrator' : 'user')}
verifier = [${verifier.join(', ')}]
created_at_ms = 0
disabled = false
revision = 1
legacy = false
initial_secret_pending = false
`;
}

async function stop(child: ChildProcess): Promise<void> {
	if (child.exitCode !== null || child.signalCode !== null) return;
	const exited = once(child, 'exit');
	child.kill('SIGINT');
	const timer = setTimeout(() => child.kill('SIGKILL'), 5000);
	try {
		await exited;
	} finally {
		clearTimeout(timer);
	}
}

async function writeFixtureConfiguration(options: {
	directory: string;
	origin: string;
	port: number;
	configurations: string[];
}): Promise<string> {
	const { directory, origin, port, configurations } = options;
	const recordings = resolve(directory, 'recordings');
	await mkdir(recordings);
	const configPath = resolve(directory, 'config.toml');
	await writeFile(
		configPath,
		`host = "127.0.0.1"
port = ${port}
[access]
local_networks = []
require_secure_remote = false
[direct_card]
allowed_origins = [${JSON.stringify(origin)}]
[storage]
medium_term_path = ${JSON.stringify(recordings)}
long_term_path = ${JSON.stringify(recordings)}
recording_catalog_path = ${JSON.stringify(resolve(directory, 'recordings.db'))}
event_thumbnail_path = ${JSON.stringify(resolve(directory, 'thumbnails'))}
short_term_secs = 5
medium_term_secs = 60
long_term_max_gb = 0
[access_credentials]
version = 1
${[...cardTestKeys, cardAdminKey].map((key) => credentialRecord(key, key === cardAdminKey)).join('\n')}
${configurations.join('\n')}`,
		{ mode: 0o600 }
	);
	return configPath;
}

async function waitForFixture(server: ChildProcess, url: string): Promise<void> {
	for (let attempt = 0; attempt < 100; attempt += 1) {
		if (server.exitCode !== null)
			throw new Error('The isolated KeepPeek fixture exited before readiness.');
		try {
			const response = await fetch(`${url}/metrics`, {
				headers: { Authorization: `Bearer ${cardAdminKey}` },
				signal: AbortSignal.timeout(500)
			});
			await response.body?.cancel();
			if (response.ok) return;
		} catch (error) {
			if (!(error instanceof TypeError) && !(error instanceof DOMException)) throw error;
		}
		await delay(100);
	}
	throw new Error('The isolated KeepPeek fixture did not become ready.');
}

export async function startHomeAssistantServer(origin: string) {
	const extension = process.platform === 'win32' ? '.exe' : '';
	const binary = (name: string) => resolve(repositoryRoot, 'target/release', `${name}${extension}`);
	await Promise.all([access(binary('keeppeek')), access(binary('test_camera'))]);
	const directory = await mkdtemp(resolve(repositoryRoot, 'target/home-assistant-e2e-'));
	const children: ChildProcess[] = [];
	const close = async () => {
		await Promise.all(children.map(stop));
		await rm(directory, { recursive: true, force: true });
	};
	try {
		const configurations: string[] = [];
		const media = resolve(repositoryRoot, 'crates/test-camera/testdata/cc-4k-640x360-h264.mp4');
		for (const [index, sourceId] of cardSourceIds.entries()) {
			const child = spawn(
				binary('test_camera'),
				[
					'rtsp',
					'--main',
					media,
					'--sub',
					media,
					'--start-at-seconds',
					'0',
					'--config-ip',
					sourceId,
					'--name',
					`card-camera-${index}`
				],
				{ stdio: ['ignore', 'pipe', 'ignore'] }
			);
			children.push(child);
			await once(child, 'spawn');
			configurations.push(await cameraConfig(child));
		}
		const port = await availablePort();
		const configPath = await writeFixtureConfiguration({ directory, origin, port, configurations });
		const server = spawn(binary('keeppeek'), [`--config=${configPath}`], {
			cwd: directory,
			env: { ...process.env, RUST_LOG: 'warn' },
			stdio: ['ignore', 'ignore', 'pipe']
		});
		children.push(server);
		let log = '';
		server.stderr?.on('data', (chunk: Buffer) => {
			log = (log + chunk.toString()).slice(-16_384);
		});
		await once(server, 'spawn');
		const url = `http://127.0.0.1:${port}`;
		await waitForFixture(server, url);
		return {
			url,
			close,
			logs: () =>
				[...cardTestKeys, cardAdminKey].reduce(
					(text, key) => text.replaceAll(key, '[redacted]'),
					log
				)
		};
	} catch (error) {
		await close();
		throw error;
	}
}
