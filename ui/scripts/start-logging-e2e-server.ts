import { mkdir, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';

const repositoryRoot = path.resolve(import.meta.dir, '../..');
const testRoot = path.join(repositoryRoot, 'target', 'ui-logging-e2e');
const storageRoot = path.join(testRoot, 'recordings');
const configPath = path.join(testRoot, 'config.toml');

await rm(testRoot, { recursive: true, force: true });
await mkdir(storageRoot, { recursive: true });

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
}

process.once('SIGINT', stopServer);
process.once('SIGTERM', stopServer);
process.once('exit', stopServer);

const exitCode = await server.exited;
process.exitCode = stopping ? 0 : exitCode;
