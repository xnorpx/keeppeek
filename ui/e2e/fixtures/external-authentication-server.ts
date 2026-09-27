import { spawn, execFile, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:net';
import { resolve } from 'node:path';
import { StringDecoder } from 'node:string_decoder';

export type AuthenticationFixtureCommand =
	'outage' | 'healthy' | 'user' | 'administrator' | 'revoke' | 'proxy' | 'oidc';
type Manifest = { backend: string; issuer: string; certificate_path: string; key_path: string };
const repositoryRoot = resolve(import.meta.dirname, '../../..');

class ProtocolChild {
	readonly child: ChildProcessWithoutNullStreams;
	private lines: string[] = [];
	private pending: {
		prefix: string;
		resolve: (value: string) => void;
		reject: (error: Error) => void;
	} | null = null;
	private failed = false;
	private phase = 'child startup';

	constructor(executable: string, args: string[], env: NodeJS.ProcessEnv) {
		this.child = spawn(executable, args, {
			cwd: repositoryRoot,
			env,
			stdio: 'pipe',
			windowsHide: true,
			detached: process.platform !== 'win32'
		});
		const decoder = new StringDecoder('utf8');
		let buffer = '';
		this.child.stdout.on('data', (chunk: Buffer) => {
			buffer += decoder.write(chunk);
			if (buffer.length > 4 * 1024 * 1024) {
				this.fail();
				return;
			}
			let newline: number;
			while ((newline = buffer.indexOf('\n')) >= 0) {
				const line = buffer.slice(0, newline).trim();
				buffer = buffer.slice(newline + 1);
				const marker = line.indexOf('KEEPPEEK_AUTH_');
				if (marker >= 0) this.accept(line.slice(marker));
			}
		});
		// Retain only fixed build-stage labels, never raw provider diagnostics.
		this.child.stderr.on('data', (chunk: Buffer) => {
			const text = chunk.toString();
			const panicLocation = /panicked at (src[/\\][\w/\\.]+:\d+:\d+)/.exec(text)?.[1];
			if (panicLocation) this.phase = `Rust panic at ${panicLocation}`;
			else if (text.includes('Blocking waiting for file lock')) this.phase = 'Cargo build lock';
			else if (text.includes('Compiling ')) this.phase = 'Rust compilation';
			else if (text.includes('Running unittests')) this.phase = 'Rust fixture initialization';
		});
		this.child.stdin.on('error', () => this.fail());
		this.child.on('error', () => this.fail());
		this.child.on('exit', () => this.fail());
	}

	private fail(): void {
		this.failed = true;
		this.pending?.reject(
			new Error(
				`Isolated authentication fixture exited or violated its output bound (${this.phase}).`
			)
		);
		this.pending = null;
	}
	private accept(line: string): void {
		if (this.pending && line.startsWith(this.pending.prefix)) {
			this.pending.resolve(line.slice(this.pending.prefix.length));
			this.pending = null;
		} else if (this.lines.length < 8) this.lines.push(line);
		else this.fail();
	}
	async wait(prefix: string, timeoutMs = 30_000): Promise<string> {
		if (this.failed || this.pending)
			throw new Error('Fixture is unavailable or a command is already pending.');
		const index = this.lines.findIndex((line) => line.startsWith(prefix));
		if (index >= 0) return this.lines.splice(index, 1)[0].slice(prefix.length);
		let timer: ReturnType<typeof setTimeout> | undefined;
		try {
			return await new Promise<string>((resolveLine, reject) => {
				this.pending = { prefix, resolve: resolveLine, reject };
				timer = setTimeout(
					() =>
						reject(
							new Error(
								`Authentication fixture timed out during ${this.phase}, waiting for ${prefix.trim()}.`
							)
						),
					timeoutMs
				);
			});
		} finally {
			clearTimeout(timer);
			this.pending = null;
		}
	}
	async command(command: string, prefix: string, timeoutMs = 30_000): Promise<void> {
		const acknowledgement = this.wait(`${prefix}${command}`, timeoutMs);
		this.child.stdin.write(`${JSON.stringify({ command })}\n`);
		if ((await acknowledgement) !== '')
			throw new Error('Unexpected authentication fixture acknowledgement.');
	}
	async audit(prefix = 'KEEPPEEK_AUTH_'): Promise<{ audit: unknown; logs: unknown }> {
		const result = this.wait(`${prefix}AUDIT `);
		this.child.stdin.write(`${JSON.stringify({ command: 'audit' })}\n`);
		const value: unknown = JSON.parse(await result);
		await this.wait(`${prefix}ACK audit`);
		if (!value || typeof value !== 'object' || !('audit' in value) || !('logs' in value))
			throw new Error('Invalid fixture audit response.');
		return { audit: value.audit, logs: value.logs };
	}
	async close(prefix: string): Promise<void> {
		if (!this.child.pid || this.child.exitCode !== null || this.child.signalCode !== null) return;
		try {
			await this.command('stop', prefix, 5000);
		} catch {
			/* A crashed child cannot acknowledge shutdown. */
		}
		if (this.child.exitCode !== null || this.child.signalCode !== null) return;
		const exit = once(this.child, 'exit');
		let deadline: ReturnType<typeof setTimeout> | undefined;
		const timer = setTimeout(() => {
			const pid = this.child.pid;
			if (!pid) return;
			if (process.platform === 'win32')
				execFile('taskkill', ['/PID', String(pid), '/T', '/F'], { windowsHide: true }, () => {});
			else {
				try {
					process.kill(-pid, 'SIGKILL');
				} catch {
					this.child.kill('SIGKILL');
				}
			}
		}, 5000);
		try {
			await Promise.race([
				exit,
				new Promise<never>((_, reject) => {
					deadline = setTimeout(
						() => reject(new Error('Authentication fixture did not terminate.')),
						10_000
					);
				})
			]);
		} finally {
			clearTimeout(timer);
			clearTimeout(deadline);
		}
	}
}

function manifest(value: string): Manifest {
	const parsed: unknown = JSON.parse(value);
	if (!parsed || typeof parsed !== 'object')
		throw new Error('Invalid authentication fixture manifest.');
	for (const field of ['backend', 'issuer', 'certificate_path', 'key_path']) {
		if (!(field in parsed) || typeof Reflect.get(parsed, field) !== 'string')
			throw new Error('Incomplete authentication fixture manifest.');
	}
	const result = parsed as Manifest;
	if (
		new URL(result.backend).hostname !== '127.0.0.1' ||
		new URL(result.backend).protocol !== 'http:' ||
		new URL(result.issuer).hostname !== '127.0.0.1' ||
		new URL(result.issuer).protocol !== 'https:'
	)
		throw new Error('Authentication fixture must use isolated loopback endpoints.');
	return result;
}

function startTlsProxy(port: number, endpoints: Manifest): ProtocolChild {
	return new ProtocolChild(
		'bun',
		[
			resolve(import.meta.dirname, 'external-authentication-proxy.ts'),
			String(port),
			endpoints.backend,
			endpoints.certificate_path,
			endpoints.key_path
		],
		process.env
	);
}

function startRustFixture(origin: string): ProtocolChild {
	// A precompiled binary keeps build-lock waits outside the browser runtime deadline.
	const executable = process.env.KEEPPEEK_AUTH_E2E_BINARY;
	const testArgs = ['issue123_browser_fixture', '--ignored', '--nocapture', '--test-threads=1'];
	return new ProtocolChild(
		executable || 'cargo',
		executable
			? testArgs
			: ['test', '--locked', '--lib', 'issue123_browser_fixture', '--', ...testArgs.slice(1)],
		{ ...process.env, KEEPPEEK_AUTH_E2E_ORIGIN: origin }
	);
}

export async function startAuthenticationFixture() {
	const reservation = createServer();
	reservation.listen(0, '127.0.0.1');
	await once(reservation, 'listening');
	const address = reservation.address();
	if (!address || typeof address === 'string') throw new Error('Fixture port allocation failed.');
	const origin = `https://127.0.0.1:${address.port}`;
	let reserved = true;
	const release = async () => {
		if (!reserved) return;
		reserved = false;
		await new Promise<void>((done, reject) =>
			reservation.close((error) => (error ? reject(error) : done()))
		);
	};
	const rust = startRustFixture(origin);
	let proxy: ProtocolChild | null = null;
	const close = async () => {
		await release();
		await proxy?.close('KEEPPEEK_AUTH_PROXY_ACK ');
		await rust.close('KEEPPEEK_AUTH_ACK ');
	};
	try {
		const endpoints = manifest(await rust.wait('KEEPPEEK_AUTH_FIXTURE ', 240_000));
		await release();
		proxy = startTlsProxy(address.port, endpoints);
		if ((await proxy.wait('KEEPPEEK_AUTH_PROXY_READY ')) !== String(address.port))
			throw new Error('TLS fixture bound the wrong port.');
		process.stdout.write('Authentication TLS fixture ready; serving the production embedded UI.\n');
		let commanding = false;
		return {
			origin,
			issuer: endpoints.issuer,
			close,
			async audit() {
				const observed = await proxy!.audit('KEEPPEEK_AUTH_PROXY_');
				if (!Array.isArray(observed.audit) || observed.audit.length !== 0)
					throw new Error(
						'TLS proxy observed private material or an oversized authentication response.'
					);
				return rust.audit();
			},
			async command(command: AuthenticationFixtureCommand) {
				if (commanding) throw new Error('Authentication fixture commands must be sequential.');
				commanding = true;
				try {
					await rust.command(command, 'KEEPPEEK_AUTH_ACK ');
					if (['proxy', 'oidc', 'user', 'administrator'].includes(command))
						await proxy!.command(command, 'KEEPPEEK_AUTH_PROXY_ACK ');
				} finally {
					commanding = false;
				}
			}
		};
	} catch (error) {
		await close();
		throw error;
	}
}

export type AuthenticationFixture = Awaited<ReturnType<typeof startAuthenticationFixture>>;
