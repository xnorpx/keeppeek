import { expect, test } from '@playwright/test';
import { gzipSync } from 'node:zlib';
import { reportingRequestHandler, reportingService } from './reporting-service';

const day = 86_400_000;

function setup() {
	let now = 0;
	const privateCanary = 'local-private-canary';
	const service = reportingService(() => now, [privateCanary]);
	const diagnostics = new Blob([gzipSync(JSON.stringify({ internal: privateCanary }))], {
		type: 'application/gzip'
	});
	const screen = new Blob([privateCanary], { type: 'video/webm' });
	const manifest = {
		version: 1,
		title: 'Cannot open a clip',
		description: 'The clip remains paused.',
		reproduction: 'Open the sample clip from the recording list.',
		expected: 'The clip starts playing.',
		actual: 'The player remains paused.',
		evidenceWarnings: ['server-log-filter-active'],
		artifacts: {
			diagnostics: { type: diagnostics.type, bytes: diagnostics.size },
			screen: { type: screen.type, bytes: screen.size }
		}
	};
	const create = () => service.create(service.recorderKey, 'report-one', manifest, 30);
	return {
		service,
		diagnostics,
		screen,
		manifest,
		create,
		advance: (ms: number) => {
			now += ms;
		}
	};
}

test('one confirmed report creates one issue and private evidence expires', async () => {
	const { service, diagnostics, screen, create, advance } = setup();
	const { id, grant } = create();
	expect(create()).toEqual({ id, grant, expires: 300_000 });
	expect(service.status(id, grant)).toEqual({ state: 'pending' });
	await service.upload(id, grant, 'diagnostics', diagnostics);
	expect(() => service.finalize(id, grant)).toThrow('incomplete-uploads');
	await service.upload(id, grant, 'screen', screen);
	await service.upload(id, grant, 'screen', screen);
	const result = service.finalize(id, grant);
	expect(service.finalize(id, grant)).toEqual(result);
	expect(result).toMatchObject({
		state: 'complete',
		issueUrl: 'https://issues.invalid/keeppeek/1'
	});
	expect(service.createdIssues()).toHaveLength(1);
	expect(service.createdIssues()[0]).toContain('server-log-filter-active');
	expect(service.createdIssues()[0]).not.toContain('local-private-canary');
	expect(service.createdIssues()[0]).not.toContain(grant);
	expect(() => service.read(id, grant, 'screen')).toThrow('unauthorized');
	expect(service.read(id, service.maintainerKey, 'screen')).toBe(screen);
	advance(30 * day);
	service.sweep();
	expect(service.read(id, service.maintainerKey, 'screen')).toBeNull();
	expect(service.read(id, service.maintainerKey, 'diagnostics')).toBeNull();
});

test('rejects invalid metadata, incorrect uploads, and expired grants', async () => {
	const { service, manifest, create, advance, diagnostics } = setup();
	expect(() => service.create('', 'report-one', manifest, 1)).toThrow('unauthorized');
	expect(() =>
		service.create(service.recorderKey, 'report-one', { ...manifest, grant: 'hidden' }, 1)
	).toThrow('invalid-manifest');
	const tooLarge = {
		...manifest,
		artifacts: { diagnostics: { type: 'application/gzip', bytes: 130 * 2 ** 20 } }
	};
	expect(() => service.create(service.recorderKey, 'report-one', tooLarge, 1)).toThrow(
		'invalid-manifest'
	);
	expect(() => service.create(service.recorderKey, 'report-one', manifest, 31)).toThrow(
		'invalid-retention'
	);

	const { id, grant } = create();
	expect(() =>
		service.create(service.recorderKey, 'report-one', { ...manifest, title: 'Different' }, 30)
	).toThrow('idempotency-conflict');
	await expect(
		service.upload(id, grant, 'diagnostics', new Blob(['bad'], { type: 'text/plain' }))
	).rejects.toThrow('artifact-type-mismatch');
	await expect(
		service.upload(id, grant, 'diagnostics', new Blob(['bad'], { type: 'application/gzip' }))
	).rejects.toThrow('artifact-size-mismatch');
	await service.upload(id, grant, 'diagnostics', diagnostics);
	const changed = new Blob([new Uint8Array(diagnostics.size)], { type: diagnostics.type });
	await expect(service.upload(id, grant, 'diagnostics', changed)).rejects.toThrow(
		'artifact-conflict'
	);
	advance(300_000);
	expect(() => service.finalize(id, grant)).toThrow('expired-grant');
	expect(service.createdIssues()).toEqual([]);
});

test('rejects private public text and allows staged reports to be cancelled', async () => {
	const { service, diagnostics, screen, manifest } = setup();
	const { id, grant } = service.create(
		service.recorderKey,
		'report-private',
		{ ...manifest, description: 'Includes local-private-canary' },
		1
	);
	await service.upload(id, grant, 'diagnostics', diagnostics);
	await service.upload(id, grant, 'screen', screen);
	expect(() => service.finalize(id, grant)).toThrow('private-public-output');
	expect(service.cancel(id, grant)).toEqual({ state: 'cancelled' });
	expect(service.cancel(id, grant)).toEqual({ state: 'cancelled' });
	expect(service.finalize(id, grant)).toEqual({ state: 'cancelled' });
	expect(service.createdIssues()).toEqual([]);
});

test('the reporting service wire publishes one issue and permits only the configured recorder origin', async () => {
	const { service, diagnostics, screen, create } = setup();
	const { id, grant } = create();
	const origin = 'https://recorder.invalid';
	const root = `https://reporting.invalid/v1/reports/${id}`;
	const handler = reportingRequestHandler(service, origin);
	const authorized = { Origin: origin, Authorization: `Bearer ${grant}` };
	const withArtifact = (kind: string, body: Blob) =>
		handler(
			new Request(`${root}/artifacts/${kind}`, {
				method: 'PUT',
				headers: { ...authorized, 'Content-Type': body.type },
				body
			})
		);
	const finish = () =>
		handler(new Request(`${root}/finalize`, { method: 'POST', headers: authorized }));
	expect(
		(
			await handler(
				new Request(root, { headers: { ...authorized, Origin: 'https://other.invalid' } })
			)
		).status
	).toBe(403);
	const preflight = await handler(
		new Request(`${root}/artifacts/diagnostics`, { method: 'OPTIONS', headers: { Origin: origin } })
	);
	expect(preflight.status).toBe(204);
	expect(preflight.headers.get('Access-Control-Allow-Origin')).toBe(origin);
	expect(preflight.headers.get('Access-Control-Allow-Credentials')).toBeNull();
	expect((await handler(new Request(root, { headers: { Origin: origin } }))).status).toBe(401);
	expect((await finish()).status).toBe(409);
	expect((await withArtifact('diagnostics', diagnostics)).status).toBe(204);
	expect((await withArtifact('screen', screen)).status).toBe(204);
	const result = await (await finish()).json();
	expect(result).toMatchObject({ state: 'complete' });
	expect(await (await finish()).json()).toEqual(result);
	expect(
		(await handler(new Request(`${root}/artifacts/screen`, { headers: authorized }))).status
	).toBe(405);
	expect(service.createdIssues()).toHaveLength(1);
});

test('checks upload bytes before accepting a streamed artifact and exposes no private errors', async () => {
	const { service, diagnostics, create } = setup();
	const { id, grant } = create();
	const origin = 'https://recorder.invalid';
	const handler = reportingRequestHandler(service, origin);
	const root = `https://reporting.invalid/v1/reports/${id}`;
	const headers = {
		Origin: origin,
		Authorization: `Bearer ${grant}`,
		'Content-Type': diagnostics.type
	};
	let cancelled = false;
	const body = new ReadableStream({
		start(controller) {
			controller.enqueue(new Uint8Array(diagnostics.size + 1));
		},
		cancel() {
			cancelled = true;
		}
	});
	const request: RequestInit & { duplex: 'half' } = {
		method: 'PUT',
		headers,
		body,
		duplex: 'half'
	};
	const oversized = await handler(new Request(`${root}/artifacts/diagnostics`, request));
	expect(oversized.status).toBe(413);
	expect(await oversized.text()).toBe('');
	expect(cancelled).toBe(true);
	const wrongType = await handler(
		new Request(`${root}/artifacts/diagnostics`, {
			method: 'PUT',
			headers: { ...headers, 'Content-Type': 'text/plain' },
			body: diagnostics
		})
	);
	expect(wrongType.status).toBe(415);
	expect(await wrongType.text()).toBe('');
	expect((await handler(new Request(`${root}/finalize`, { method: 'POST', headers }))).status).toBe(
		409
	);
	expect(service.createdIssues()).toHaveLength(0);
});
