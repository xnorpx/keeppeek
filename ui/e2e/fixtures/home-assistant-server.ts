import { execFile, spawn, type ChildProcess } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { access, cp, mkdir, mkdtemp, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { basename, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { promisify } from 'node:util';
import { seedCalibrationRecording } from './calibration-recording';

export const cardTestKeys = [
	'550e8400-e29b-41d4-a716-446655440001',
	'550e8400-e29b-41d4-a716-446655440002',
	'550e8400-e29b-41d4-a716-446655440003'
] as const;
export const cardAdminKey = '550e8400-e29b-41d4-a716-446655440004';
export const cardSourceIds = ['192.0.2.101', '192.0.2.102'] as const;
const repositoryRoot = resolve(import.meta.dirname, '../../..');
type FixtureOptions = {
	trustedLocal?: boolean;
	archiveFixture?: boolean;
	calibrationFixture?: { mediaPath: string };
};

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
	trustedLocal?: boolean;
	archiveFixture?: boolean;
	calibrationFixture?: { mediaPath: string };
}): Promise<string> {
	const { directory, origin, port, configurations, trustedLocal = false } = options;
	const configPath = resolve(directory, 'config.toml');
	await writeFile(
		configPath,
		`host = "127.0.0.1"
port = ${port}
[access]
local_networks = ${trustedLocal ? '["127.0.0.1/32"]' : '[]'}
require_secure_remote = false
[direct_card]
allowed_origins = [${JSON.stringify(origin)}]
[storage]
short_term_secs = 5
medium_term_secs = 60
long_term_max_gb = 0
${options.archiveFixture || options.calibrationFixture ? '\n[camera_defaults]\nrecording_mode = "off"\n' : ''}
[access_credentials]
version = 1
${[...cardTestKeys, cardAdminKey].map((key) => credentialRecord(key, key === cardAdminKey)).join('\n')}
${configurations.map((configuration, index) => fixtureCameraConfiguration(configuration, index, options)).join('\n')}`,
		{ mode: 0o600 }
	);
	return configPath;
}

function fixtureCameraConfiguration(configuration: string, index: number, options: FixtureOptions) {
	if (options.calibrationFixture) {
		return `${configuration}\nrecorded_audio = { default_offset_ms = 200 }\n`;
	}
	return options.archiveFixture && index === 1
		? `${configuration}\nrecording_mode = "sub"\n`
		: configuration;
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

async function startFixtureRecorder(options: {
	binary: string;
	configPath: string;
	directory: string;
	url: string;
	children: ChildProcess[];
	onLog: (chunk: Buffer) => void;
}): Promise<ChildProcess> {
	const server = spawn(options.binary, [`--config=${options.configPath}`], {
		cwd: options.directory,
		env: { ...process.env, RUST_LOG: 'warn' },
		stdio: ['ignore', 'ignore', 'pipe']
	});
	options.children.push(server);
	server.stderr?.on('data', options.onLog);
	await once(server, 'spawn');
	await waitForFixture(server, options.url);
	return server;
}

async function fixtureCameras(
	binary: string,
	children: ChildProcess[],
	archiveFixture: boolean
): Promise<string[]> {
	const configurations: string[] = [];
	const media = resolve(repositoryRoot, 'crates/test-camera/testdata/cc-4k-640x360-h264.mp4');
	for (const [index, sourceId] of cardSourceIds.entries()) {
		const child = spawn(
			binary,
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
				archiveFixture ? sourceId : `card-camera-${index}`
			],
			{ stdio: ['ignore', 'pipe', 'ignore'] }
		);
		children.push(child);
		await once(child, 'spawn');
		const configuration = await cameraConfig(child);
		configurations.push(
			archiveFixture ? `${configuration}\ndisplay_name = "card-camera-${index}"\n` : configuration
		);
	}
	return configurations;
}

async function prepareArchiveFixture(
	binary: string,
	directory: string,
	configPath: string,
	children: ChildProcess[],
	enabled: boolean
) {
	const archiveRoot = resolve(directory, 'fourth-tier-archive');
	let seededAtMs = Date.now() - 120_000;
	let seededObjectId = '';
	let seededObjectBytes = 0;
	if (enabled) {
		await privateArchiveDirectory(archiveRoot, directory);
		const seed = spawn(
			binary,
			[
				'seed-recording',
				'--source',
				resolve(repositoryRoot, 'crates/test-camera/testdata/cc-4k-640x360-h264.mp4'),
				'--config',
				configPath,
				'--stream-id',
				`${cardSourceIds[0]}/sub`,
				'--duration-seconds',
				'40',
				'--age-seconds',
				'120'
			],
			{ cwd: directory, stdio: 'ignore', windowsHide: true, timeout: 30_000, killSignal: 'SIGKILL' }
		);
		children.push(seed);
		const [code] = await once(seed, 'exit');
		if (code !== 0) throw new Error('The isolated archive recording seed failed.');
		seededAtMs = Date.now() - 120_000;
		const media = (await readdir(directory, { recursive: true })).filter((path) =>
			path.endsWith('.mp4')
		);
		if (media.length !== 1)
			throw new Error('The archive fixture requires exactly one seeded object before launch.');
		seededObjectId = basename(media[0]!, '.mp4');
		seededObjectBytes = (await stat(resolve(directory, media[0]!))).size;
		if (!/^[0-9a-f-]{36}$/.test(seededObjectId))
			throw new Error('The seeded recording is missing its UUID.');
	}
	return { archiveRoot, seededAtMs, seededObjectId, seededObjectBytes };
}

async function privateArchiveDirectory(root: string, directory: string) {
	if (resolve(root) !== resolve(directory, 'fourth-tier-archive'))
		throw new Error('Archive directory must be the generated fixture child.');
	await mkdir(root, { mode: 0o700 });
	if (process.platform !== 'win32') return;
	const { stdout } = await promisify(execFile)(
		'powershell.exe',
		[
			'-NoProfile',
			'-NonInteractive',
			'-Command',
			`$ErrorActionPreference = 'Stop'
$archiveBefore = Get-Acl -LiteralPath $env:KEEPPEEK_ARCHIVE_FIXTURE_ROOT
$archiveOwner = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$archiveAcl = [System.Security.AccessControl.DirectorySecurity]::new()
$archiveAcl.SetOwner($archiveOwner)
$archiveAcl.SetAccessRuleProtection($true, $false)
$archiveAcl.AddAccessRule([System.Security.AccessControl.FileSystemAccessRule]::new($archiveOwner, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
Set-Acl -LiteralPath $env:KEEPPEEK_ARCHIVE_FIXTURE_ROOT -AclObject $archiveAcl
@{before = $archiveBefore.Sddl; after = (Get-Acl -LiteralPath $env:KEEPPEEK_ARCHIVE_FIXTURE_ROOT).Sddl} | ConvertTo-Json`
		],
		{
			env: { ...process.env, KEEPPEEK_ARCHIVE_FIXTURE_ROOT: root },
			windowsHide: true,
			timeout: 10_000,
			maxBuffer: 16_384
		}
	);
	await writeFile(resolve(directory, 'archive-directory-acl.json'), stdout);
}

async function preserveFixture(server: ChildProcess, directory: string, destination: string) {
	await stop(server);
	if (process.platform === 'win32') {
		const { stdout } = await promisify(execFile)(
			'powershell.exe',
			[
				'-NoProfile',
				'-NonInteractive',
				'-Command',
				`Get-ChildItem -LiteralPath $env:KEEPPEEK_ARCHIVE_FIXTURE_ROOT -Directory -Recurse | ForEach-Object { $fixtureAcl = Get-Acl -LiteralPath $_.FullName; @{path = $_.FullName; owner = $fixtureAcl.Owner; sddl = $fixtureAcl.Sddl; protected = $fixtureAcl.AreAccessRulesProtected} } | ConvertTo-Json`
			],
			{
				env: { ...process.env, KEEPPEEK_ARCHIVE_FIXTURE_ROOT: directory },
				windowsHide: true,
				timeout: 10_000,
				maxBuffer: 65_536
			}
		);
		await writeFile(resolve(directory, 'fixture-directory-acls.json'), stdout);
	}
	await cp(directory, destination, {
		recursive: true,
		filter: (source) => !/(?:secrets\.toml|\.(?:mp4|tmp|jpg|png))$/i.test(source)
	});
}

export async function startHomeAssistantServer(origin: string, options: FixtureOptions = {}) {
	const extension = process.platform === 'win32' ? '.exe' : '';
	const binary = (name: string) => resolve(repositoryRoot, 'target/release', `${name}${extension}`);
	await Promise.all([access(binary('keeppeek')), access(binary('test_camera'))]);
	const parent = process.platform === 'win32' ? tmpdir() : resolve(repositoryRoot, 'target');
	const directory = await mkdtemp(resolve(parent, 'home-assistant-e2e-'));
	const children: ChildProcess[] = [];
	const close = async () => {
		await Promise.all(children.map(stop));
		await rm(directory, { recursive: true, force: true });
	};
	try {
		const port = await availablePort();
		const { configPath, archive, calibration } = await prepareFixture(
			binary('test_camera'),
			directory,
			origin,
			port,
			children,
			options
		);
		let log = '';
		const url = `http://127.0.0.1:${port}`;
		const launch = () =>
			startFixtureRecorder({
				binary: binary('keeppeek'),
				configPath,
				directory,
				url,
				children,
				onLog: (chunk) => {
					log = (log + chunk.toString()).slice(-16_384);
				}
			});
		let server = await launch();
		return {
			url,
			backendProcessId: () => server.pid,
			...archive,
			calibration,
			close,
			preserveArchiveEvidence: (destination: string) =>
				preserveFixture(server, directory, destination),
			restart: async () => {
				await stop(server);
				server = await launch();
			},
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

async function prepareFixture(
	binary: string,
	directory: string,
	origin: string,
	port: number,
	children: ChildProcess[],
	options: FixtureOptions
) {
	if (options.archiveFixture && options.calibrationFixture) {
		throw new Error('Archive and calibration recording seeds must use separate fixtures.');
	}
	const configurations = await fixtureCameras(
		binary,
		children,
		Boolean(options.archiveFixture || options.calibrationFixture)
	);
	const configPath = await writeFixtureConfiguration({
		directory,
		origin,
		port,
		configurations,
		...options
	});
	const archive = await prepareArchiveFixture(
		binary,
		directory,
		configPath,
		children,
		options.archiveFixture ?? false
	);
	const calibration = options.calibrationFixture
		? await seedCalibrationRecording(
				binary,
				directory,
				configPath,
				cardSourceIds[0],
				options.calibrationFixture.mediaPath
			)
		: undefined;
	return { configPath, archive, calibration };
}
