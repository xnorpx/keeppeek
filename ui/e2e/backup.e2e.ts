import { expect, test, type APIRequestContext, type Locator, type Page } from '@playwright/test';
import { execFile, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { copyFile, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { resolve } from 'node:path';
import { promisify } from 'node:util';
import { mockControlPeer } from './fixtures/control-peer';

const execFileAsync = promisify(execFile);
const keeppeekBinary = resolve(
	process.cwd(),
	`../target/release/keeppeek${process.platform === 'win32' ? '.exe' : ''}`
);
const backendURL = `http://127.0.0.1:${process.env.KEEPPEEK_E2E_BACKEND_PORT ?? '4317'}`;

test.describe.configure({ mode: 'default' });

const runtimeConfiguration = {
	host: '0.0.0.0',
	port: 3000,
	camera_count: 0,
	storage: {
		medium_term_path: '/recordings',
		long_term_path: '/recordings',
		recording_catalog_path: '/recordings/recordings.db',
		event_thumbnail_path: '/recordings/.event-thumbnails',
		event_thumbnail_max_mb: 1024,
		short_term_secs: 120,
		medium_term_secs: 1800,
		flush_interval_secs: 60,
		write_buffer_bytes: 8192,
		long_term_max_gb: 2048
	},
	recording_estimate: {
		estimated_bitrate_bps: 0,
		bytes_per_day: 0,
		known_streams: 0,
		unknown_streams: 0,
		estimated_retention_days: null
	}
};

test('CLI exports a ZIP and requires confirmation before applying it', async ({
	request
}, testInfo) => {
	const output = testInfo.outputPath('configuration.zip');
	const exported = await configCli(['export', '--output', output]);
	const bytes = await readFile(output);
	expect(exported.archiveBytes).toBe(bytes.length.toString());
	const archive = await inspectConfigurationZip(output);
	expect(archive.members.sort()).toEqual(['config.toml', 'secrets.toml']);
	await expect(configCli(['apply', output])).rejects.toMatchObject({
		code: 2,
		stdout: '',
		stderr: expect.stringContaining('--confirm')
	});
	const invalid = testInfo.outputPath('invalid.zip');
	await writeFile(invalid, 'not a ZIP archive');
	await expect(configCli(['apply', invalid, '--confirm'])).rejects.toMatchObject({
		code: 3,
		stdout: ''
	});
	expect((await request.get(`${backendURL}/api/backups`)).status()).toBe(404);
});

test('Administrator exports and applies the two-file ZIP through the direct endpoints', async ({
	page
}, testInfo) => {
	await page.setViewportSize({ width: 1440, height: 900 });
	const retiredRequests: string[] = [];
	page.on('request', (request) => {
		if (new URL(request.url()).pathname.startsWith('/api/backups'))
			retiredRequests.push(request.url());
	});
	const controls = await mockControlPeer(page, {
		runtimeConfiguration,
		capabilityIds: ['keeppeek.backup.v1']
	});
	await page.goto('/settings#backups');
	const section = page.getByRole('region', { name: 'Backup and restore' });
	await expect(section).toBeVisible();
	await expect(
		section.getByText('Backup ZIPs contain plaintext secrets.', { exact: true })
	).toBeVisible();
	await expect(
		section.getByRole('button', { name: 'Apply configuration', exact: true })
	).toBeDisabled();

	const { downloadPath, archiveBytes } = await downloadConfigurationZip(page, section);
	await expect(page).toHaveURL(/\/settings#backups$/);
	await expect(section).toBeVisible();

	await section.getByLabel('Configuration ZIP', { exact: true }).setInputFiles(downloadPath);
	await expect(
		section.getByRole('button', { name: 'Apply configuration', exact: true })
	).toBeDisabled();
	await section.getByRole('checkbox').check();
	const applyResponsePromise = page.waitForResponse(
		(response) =>
			new URL(response.url()).pathname === '/config/apply' && response.request().method() === 'POST'
	);
	await section.getByRole('button', { name: 'Apply configuration', exact: true }).click();
	const applyResponse = await applyResponsePromise;
	expect(applyResponse.status()).toBe(202);
	expect(applyResponse.headers()['cache-control']).toBe('no-store');
	expect(applyResponse.request().headers()['content-type']).toBe('application/zip');
	expect(applyResponse.request().postDataBuffer()).toEqual(archiveBytes);
	await expect(section.getByText('Configuration staged. Restart required.')).toBeVisible();
	await expect(section.getByRole('button', { name: 'Restart to apply' })).toBeEnabled();
	expect(controls.restarts).toBe(0);
	expect(retiredRequests).toEqual([]);
	await page.screenshot({ path: testInfo.outputPath('configuration-desktop.png'), fullPage: true });
	await section.getByRole('button', { name: 'Restart to apply' }).click();
	await expect.poll(() => controls.restarts).toBe(1);
});

test('configuration workflow fits mobile without horizontal overflow', async ({
	page
}, testInfo) => {
	await page.setViewportSize({ width: 390, height: 844 });
	const controls = await mockControlPeer(page, {
		runtimeConfiguration,
		capabilityIds: ['keeppeek.backup.v1']
	});
	await page.goto('/settings#backups');
	await expect(page.getByRole('region', { name: 'Backup and restore' })).toBeVisible();
	await expect
		.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
		.toBe(true);
	await page.screenshot({ path: testInfo.outputPath('configuration-mobile.png'), fullPage: true });
	await controls.publishCapabilities([]);
	await expect(page.getByRole('region', { name: 'Backup and restore' })).toHaveCount(0);
});

test('invalid configuration ZIP keeps the selected file and exposes a retryable error', async ({
	page
}) => {
	await mockControlPeer(page, {
		runtimeConfiguration,
		capabilityIds: ['keeppeek.backup.v1']
	});
	await page.goto('/settings#backups');
	const section = page.getByRole('region', { name: 'Backup and restore' });
	await section.getByLabel('Configuration ZIP', { exact: true }).setInputFiles({
		name: 'invalid.zip',
		mimeType: 'application/zip',
		buffer: Buffer.from('not a ZIP archive')
	});
	await section.getByRole('checkbox').check();
	await section.getByRole('button', { name: 'Apply configuration', exact: true }).click();
	await expect(section.getByRole('alert')).toContainText('configuration archive failed validation');
	await expect(section.getByText('invalid.zip', { exact: true })).toBeVisible();
	await expect(
		section.getByRole('button', { name: 'Apply configuration', exact: true })
	).toBeEnabled();
});

test('backup workflow remains hidden from User principals', async ({ page }) => {
	await mockControlPeer(page, {
		runtimeConfiguration,
		accessRole: 'user',
		capabilityIds: ['keeppeek.backup.v1']
	});
	await page.goto('/settings#backups');
	await expect(page.getByRole('region', { name: 'Backup and restore' })).toHaveCount(0);
});

test('HTTP apply replaces both TOMLs only after an isolated recorder restart', async ({
	request
}, testInfo) => {
	test.setTimeout(90_000);
	const directory = testInfo.outputPath('isolated-recorder');
	const configPath = resolve(directory, 'config.toml');
	const secretsPath = resolve(directory, 'secrets.toml');
	const recordings = resolve(directory, 'target-recordings');
	const sourceRecordings = resolve(directory, 'source-recordings');
	const sourceZip = testInfo.outputPath('source.zip');
	const restoredZip = testInfo.outputPath('restored.zip');
	const port = await unusedPort();
	const serverURL = `http://127.0.0.1:${port}`;
	let stop: (() => Promise<void>) | undefined;
	try {
		await writeRecorderConfiguration(directory, port, sourceRecordings, 17, 'exported-value');
		stop = await startRecorder(configPath, serverURL, request);
		const exported = await exportConfigurationZip(request, serverURL, sourceZip);
		expect(exported.members.sort()).toEqual(['config.toml', 'secrets.toml']);
		expect(exported.configSha256).toBe(sha256(await readFile(configPath)));
		expect(exported.secretsSha256).toBe(sha256(await readFile(secretsPath)));
		await stop();

		await writeRecorderConfiguration(directory, port, recordings, 53, 'original-value');
		stop = await startRecorder(configPath, serverURL, request);
		const catalogPath = resolve(recordings, 'recordings.db');
		const mediaPath = resolve(recordings, 'preserved.mp4');
		await copyFile(resolve('../crates/test-camera/testdata/cc-4k-640x360-h264.mp4'), mediaPath);
		const original = await recorderFileChecksums(directory, recordings);

		const applied = await request.post(`${serverURL}/config/apply`, {
			headers: { 'Content-Type': 'application/zip' },
			data: exported.bytes
		});
		expect(applied.status()).toBe(202);
		expect(await applied.json()).toMatchObject({ state: 'RESTORE_STATE_AWAITING_RESTART' });
		expect(await recorderFileChecksums(directory, recordings)).toEqual(original);
		await stop();

		stop = await startRecorder(configPath, serverURL, request);
		const restored = await exportConfigurationZip(request, serverURL, restoredZip);
		expect(restored.storage).toMatchObject({
			short_term_secs: 17,
			long_term_path: recordings,
			recording_catalog_path: catalogPath
		});
		expect(restored.secretsSha256).toBe(exported.secretsSha256);
		expect(restored.secretsSha256).not.toBe(original.secrets);
		expect(restored.members.sort()).toEqual(['config.toml', 'secrets.toml']);
		const activated = await recorderFileChecksums(directory, recordings);
		expect(activated.catalog).toBe(original.catalog);
		expect(activated.media).toBe(original.media);
	} finally {
		await stop?.();
		await rm(directory, { recursive: true, force: true });
	}
});

async function configCli(command: string[]): Promise<Record<string, unknown>> {
	const result = await execFileAsync(
		keeppeekBinary,
		['config', '--server', backendURL, ...command],
		{ maxBuffer: 32 * 1024 * 1024 }
	);
	expect(result.stderr).toBe('');
	return JSON.parse(result.stdout) as Record<string, unknown>;
}

async function inspectConfigurationZip(archivePath: string): Promise<{
	members: string[];
	configSha256: string;
	secretsSha256: string;
	storage: { short_term_secs: number; long_term_path: string; recording_catalog_path: string };
}> {
	const python =
		process.env.KEEPPEEK_PYTHON ?? (process.platform === 'win32' ? 'python' : 'python3.12');
	const { stdout } = await execFileAsync(
		python,
		[
			'-c',
			`import hashlib, json, sys, tomllib, zipfile
with zipfile.ZipFile(sys.argv[1]) as archive:
    assert len(archive.infolist()) <= 64
    assert all(member.file_size <= 16 * 1024 * 1024 for member in archive.infolist())
    print(json.dumps({
        "members": archive.namelist(),
        "configSha256": hashlib.sha256(archive.read("config.toml")).hexdigest(),
        "secretsSha256": hashlib.sha256(archive.read("secrets.toml")).hexdigest(),
        "storage": tomllib.loads(archive.read("config.toml").decode("utf-8")).get("storage", {}),
    }))`,
			archivePath
		],
		{ timeout: 10_000, maxBuffer: 64 * 1024 }
	);
	return JSON.parse(stdout);
}

async function downloadConfigurationZip(page: Page, section: Locator) {
	const responsePromise = page.waitForResponse(
		(response) =>
			new URL(response.url()).pathname === '/config/export' && response.request().method() === 'GET'
	);
	const downloadPromise = page.waitForEvent('download');
	await section.getByRole('button', { name: 'Export ZIP', exact: true }).click();
	const response = await responsePromise;
	expect(response.status()).toBe(200);
	expect(response.headers()['content-type']).toBe('application/zip');
	expect(response.headers()['cache-control']).toBe('no-store');
	const download = await downloadPromise;
	expect(download.suggestedFilename()).toMatch(/^keeppeek-config-.*\.zip$/);
	const downloadPath = await download.path();
	if (!downloadPath) throw new Error('Configuration export did not create a download.');
	const archive = await inspectConfigurationZip(downloadPath);
	expect(archive.members.sort()).toEqual(['config.toml', 'secrets.toml']);
	return { downloadPath, archiveBytes: await readFile(downloadPath) };
}

async function recorderFileChecksums(directory: string, recordings: string) {
	const [config, secrets, catalog, media] = await Promise.all([
		readFile(resolve(directory, 'config.toml')),
		readFile(resolve(directory, 'secrets.toml')),
		readFile(resolve(recordings, 'recordings.db')),
		readFile(resolve(recordings, 'preserved.mp4'))
	]);
	return {
		config: sha256(config),
		secrets: sha256(secrets),
		catalog: sha256(catalog),
		media: sha256(media)
	};
}

async function exportConfigurationZip(
	request: APIRequestContext,
	serverURL: string,
	archivePath: string
) {
	const response = await request.get(`${serverURL}/config/export`);
	expect(response.status()).toBe(200);
	expect(response.headers()['content-type']).toBe('application/zip');
	const bytes = await response.body();
	await writeFile(archivePath, bytes, { mode: 0o600 });
	return { ...(await inspectConfigurationZip(archivePath)), bytes };
}

function sha256(bytes: Buffer): string {
	return createHash('sha256').update(bytes).digest('hex');
}

async function writeRecorderConfiguration(
	directory: string,
	port: number,
	recordings: string,
	shortTermSecs: number,
	token: string
) {
	await mkdir(recordings, { recursive: true, mode: 0o700 });
	await writeFile(
		resolve(directory, 'config.toml'),
		`host = "127.0.0.1"
port = ${port}
access_key = "{secret:KEEPPEEK_ACCESS_KEY}"

[storage]
medium_term_path = ${JSON.stringify(recordings)}
long_term_path = ${JSON.stringify(recordings)}
recording_catalog_path = ${JSON.stringify(resolve(recordings, 'recordings.db'))}
short_term_secs = ${shortTermSecs}
long_term_max_gb = 0
minimum_free_gb = 0
warning_free_gb = 0
critical_free_gb = 0
cleanup_hysteresis_gb = 0
`,
		{ mode: 0o600 }
	);
	await writeFile(
		resolve(directory, 'secrets.toml'),
		`KEEPPEEK_ACCESS_KEY = "00000000-0000-4000-8000-000000000123"
CONFIGURATION_TEST_TOKEN = ${JSON.stringify(token)}
`,
		{ mode: 0o600 }
	);
}

async function unusedPort(): Promise<number> {
	const listener = createServer();
	listener.listen(0, '127.0.0.1');
	await once(listener, 'listening');
	const address = listener.address();
	await new Promise<void>((done, reject) =>
		listener.close((error) => (error ? reject(error) : done()))
	);
	if (!address || typeof address === 'string')
		throw new Error('No isolated recorder port was allocated.');
	return address.port;
}

async function startRecorder(
	configPath: string,
	serverURL: string,
	request: APIRequestContext
): Promise<() => Promise<void>> {
	const child = spawn(keeppeekBinary, ['--config', configPath], {
		env: { ...process.env, RUST_LOG: 'error' },
		stdio: ['ignore', 'ignore', 'pipe']
	});
	let stderr = '';
	let startupError: Error | undefined;
	child.stderr.on('data', (chunk: Buffer) => {
		stderr = (stderr + chunk.toString()).slice(-4096);
	});
	child.on('error', (error) => {
		startupError = error;
	});
	const closed = new Promise<void>((done) => child.once('close', () => done()));
	const stop = async () => {
		const deadline = setTimeout(() => child.kill('SIGKILL'), 5000);
		try {
			child.kill('SIGTERM');
			await closed;
		} finally {
			clearTimeout(deadline);
		}
	};
	try {
		await expect
			.poll(
				async () => {
					if (startupError) throw startupError;
					if (child.exitCode !== null || child.signalCode !== null) {
						throw new Error(`Isolated recorder exited before readiness: ${stderr}`);
					}
					const response = await request
						.get(`${serverURL}/metrics`, { timeout: 1000 })
						.catch(() => null);
					const ready = response?.status() === 200;
					await response?.dispose();
					return ready;
				},
				{ timeout: 20_000, message: 'The isolated recorder must become ready.' }
			)
			.toBe(true);
		return stop;
	} catch (error) {
		await stop();
		throw error;
	}
}
