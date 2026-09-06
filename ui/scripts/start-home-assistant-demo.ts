import { resolve } from 'node:path';
import { createServer } from 'vite';
import {
	cardSourceIds,
	cardTestKeys,
	startHomeAssistantServer
} from '../e2e/fixtures/home-assistant-server';

let fixture: Awaited<ReturnType<typeof startHomeAssistantServer>> | undefined;
const server = await createServer({
	configFile: resolve(import.meta.dirname, '../vite.home-assistant.config.ts'),
	server: { host: '127.0.0.1', port: 49563, strictPort: false },
	plugins: [
		{
			name: 'keeppeek-local-card-fixture',
			configureServer(vite) {
				vite.middlewares.use('/__keeppeek_fixture', (request, response) => {
					response.setHeader('Cache-Control', 'no-store');
					response.setHeader('Content-Type', 'application/json');
					if (!fixture || request.method !== 'GET') {
						response.statusCode = 503;
						response.end('{}');
						return;
					}
					response.end(
						JSON.stringify({
							type: 'custom:keeppeek-card',
							endpoint: fixture.url,
							token: cardTestKeys[0],
							sources: cardSourceIds.map((source_id, index) => ({
								source_id,
								title: index === 0 ? 'Front entrance' : 'Side entrance'
							}))
						})
					);
				});
			}
		}
	]
});

let stopping = false;
async function stop(): Promise<void> {
	if (stopping) return;
	stopping = true;
	await Promise.all([server.close(), fixture?.close()]);
}
for (const signal of ['SIGINT', 'SIGTERM'] as const) {
	process.once(signal, () => {
		void stop().then(
			() => process.exit(0),
			() => process.exit(1)
		);
	});
}

try {
	await server.listen();
	const address = server.httpServer?.address();
	if (!address || typeof address === 'string') throw new Error('No demo listener was allocated.');
	const origin = `http://127.0.0.1:${address.port}`;
	fixture = await startHomeAssistantServer(origin);
	console.log(`Home Assistant card harness: ${origin}/home-assistant.html`);
} catch {
	await stop();
	console.error('The card demo could not start. Prepare the release E2E binaries and retry.');
	process.exitCode = 1;
}
