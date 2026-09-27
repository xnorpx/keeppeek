import { readFileSync } from 'node:fs';
import { createInterface } from 'node:readline';

// This child runs with Bun; the E2E typecheck intentionally uses only Node and DOM types.
declare const Bun: {
	serve(options: {
		hostname: string;
		port: number;
		tls: { cert: string; key: string };
		fetch: (request: Request) => Promise<Response>;
	}): { port: number; stop(closeActiveConnections: boolean): void | Promise<void> };
};

const [portText, backendText, certificatePath, keyPath] = process.argv.slice(2);
const port = Number(portText);
const backend = new URL(backendText);
if (
	!Number.isInteger(port) ||
	port < 1 ||
	port > 65535 ||
	backend.protocol !== 'http:' ||
	backend.hostname !== '127.0.0.1'
)
	throw new Error('Invalid isolated authentication proxy arguments.');
let proxy = false;
let role: 'user' | 'administrator' = 'user';
const responseLeaks = new Set<string>();

async function inspectResponse(response: Response, pathname: string): Promise<BodyInit | null> {
	if (!pathname.startsWith('/auth/') && !['/', '/create', '/delete'].includes(pathname))
		return response.body;
	if (!response.body) return null;
	const reader = response.body.getReader();
	const chunks: Uint8Array[] = [];
	let size = 0;
	while (true) {
		const chunk = await reader.read();
		if (chunk.done) break;
		size += chunk.value.length;
		if (size > 131072) {
			await reader.cancel();
			responseLeaks.add('authentication response exceeded bound');
			throw new Error('Authentication response exceeded its bound.');
		}
		chunks.push(chunk.value);
	}
	const body = Buffer.concat(chunks);
	const text = body.toString();
	if (
		[
			'synthetic-access-token',
			'synthetic-provider-subject-private',
			'-----BEGIN PRIVATE KEY-----',
			'synthetic-code'
		].some((marker) => text.includes(marker)) ||
		/eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}/.test(text)
	)
		responseLeaks.add('private material in application response');
	return body;
}

const server = Bun.serve({
	hostname: '127.0.0.1',
	port,
	tls: { cert: readFileSync(certificatePath, 'utf8'), key: readFileSync(keyPath, 'utf8') },
	async fetch(request) {
		const url = new URL(request.url);
		const headers = new Headers();
		for (const [name, value] of request.headers) {
			if (
				name === 'forwarded' ||
				name.startsWith('x-forwarded-') ||
				name.startsWith('x-keeppeek-') ||
				['connection', 'transfer-encoding', 'content-length', 'accept-encoding'].includes(name)
			)
				continue;
			headers.append(name, value);
		}
		headers.set('Host', url.host);
		headers.set('X-Forwarded-For', '203.0.113.1');
		headers.set('Accept-Encoding', 'identity');
		// CSRF is a browser request credential, not an identity assertion from the proxy.
		const csrf = request.headers.get('X-KeepPeek-CSRF');
		if (csrf) headers.set('X-KeepPeek-CSRF', csrf);
		if (proxy) {
			headers.set('X-KeepPeek-Subject', 'fixture-subject');
			headers.set('X-KeepPeek-Role', role);
			headers.set('X-KeepPeek-Name', 'Fixture User');
		}
		try {
			const response = await fetch(new URL(url.pathname + url.search, backend), {
				method: request.method,
				headers,
				body:
					request.method === 'GET' || request.method === 'HEAD'
						? undefined
						: await request.arrayBuffer(),
				redirect: 'manual',
				signal: AbortSignal.timeout(30_000)
			});
			const responseHeaders = new Headers(response.headers);
			responseHeaders.delete('transfer-encoding');
			// Fetch decodes upstream compression; the forwarded body has different wire bytes.
			responseHeaders.delete('content-encoding');
			responseHeaders.delete('content-length');
			return new Response(await inspectResponse(response, url.pathname), {
				status: response.status,
				headers: responseHeaders
			});
		} catch {
			return new Response('Isolated fixture backend unavailable.', { status: 502 });
		}
	}
});
process.stdout.write(`KEEPPEEK_AUTH_PROXY_READY ${server.port}\n`);
const input = createInterface({ input: process.stdin });
input.on('line', (line) => {
	if (line.length > 256) throw new Error('Fixture command exceeds its bound.');
	const value: unknown = JSON.parse(line);
	if (!value || typeof value !== 'object' || !('command' in value))
		throw new Error('Invalid fixture command.');
	const command = value.command;
	if (command === 'proxy') proxy = true;
	else if (command === 'oidc') proxy = false;
	else if (command === 'user' || command === 'administrator') role = command;
	else if (command === 'audit')
		process.stdout.write(
			`KEEPPEEK_AUTH_PROXY_AUDIT ${JSON.stringify({ audit: [...responseLeaks], logs: [] })}\n`
		);
	else if (command !== 'stop') throw new Error('Unknown fixture command.');
	process.stdout.write(`KEEPPEEK_AUTH_PROXY_ACK ${command}\n`);
	if (command === 'stop') {
		input.close();
		void Promise.resolve(server.stop(true)).then(() => process.exit(0));
	}
});
input.on('close', () => {
	void server.stop(true);
});
