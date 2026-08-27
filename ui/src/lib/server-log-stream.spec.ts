import { describe, expect, it } from 'vitest';
import { ServerLogStream, type LogStreamState } from './server-log-stream';

function streamedResponse(chunks: string[]): Response {
	const encoder = new TextEncoder();
	return new Response(
		new ReadableStream<Uint8Array>({
			start(controller) {
				for (const chunk of chunks) controller.enqueue(encoder.encode(chunk));
				controller.close();
			}
		}),
		{ status: 200, headers: { 'Content-Type': 'text/event-stream' } }
	);
}

describe('ServerLogStream', () => {
	it('parses split authenticated SSE frames and retains the reconnect cursor', async () => {
		const states: LogStreamState[] = [];
		const entries: number[] = [];
		const gaps: number[] = [];
		let replayTruncated = 0;
		const urls: string[] = [];
		let resolveReconnect: () => void = () => {};
		const reconnecting = new Promise<void>((resolve) => {
			resolveReconnect = resolve;
		});
		const open = async (url: string) => {
			urls.push(url);
			return streamedResponse([
				'id: 42\nevent: log\ndata: {"sequence":42,"timestamp_ms":1,',
				'"level":"info","target":"keeppeek","message":"ready","fields":{}}\n\n',
				'event: gap\ndata: {"dropped":3}\n\n',
				'event: replay-truncated\ndata: {}\n\n'
			]);
		};
		const stream = new ServerLogStream(
			{
				onentry: (entry) => entries.push(entry.sequence),
				onstate: (state) => {
					states.push(state);
					if (state === 'reconnecting') resolveReconnect();
				},
				ongap: (dropped) => gaps.push(dropped),
				onreplaytruncated: () => (replayTruncated += 1)
			},
			open
		);

		stream.start(undefined, 50);
		await reconnecting;
		stream.close();

		expect(urls).toEqual(['/logs?tail=50']);
		expect(entries).toEqual([42]);
		expect(gaps).toEqual([3]);
		expect(replayTruncated).toBe(1);
		expect(states).toEqual(['connecting', 'connected', 'reconnecting', 'closed']);
	});
});
