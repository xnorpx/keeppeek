import { expect, it } from 'vitest';
import { reportingService } from '../../e2e/reporting-service';
import { ReportSubmission } from './report-submission';
import { createRemoteReportTransport } from './report-transport';

it('uploads only selected artifacts to the configured origin with a scoped grant', async () => {
	const service = reportingService(() => 5, []);
	const requests: string[] = [];
	const request = (async (input, init) => {
		const url = new URL(String(input));
		const headers = new Headers(init?.headers);
		const token = headers.get('Authorization')?.replace(/^Bearer /, '') ?? '';
		expect(url.origin).toBe('https://reporting.invalid');
		expect(init?.credentials).toBe('omit');
		expect(init?.redirect).toBe('error');
		expect(init?.referrerPolicy).toBe('no-referrer');
		expect(url.search).toBe('');
		const [, , , id, action, kind] = url.pathname.split('/');
		requests.push(`${init?.method} ${action ?? ''}`);
		if (init?.method === 'GET') return Response.json(service.status(id, token));
		if (init?.method === 'POST') return Response.json(service.finalize(id, token));
		if (init?.method === 'PUT' && kind === 'diagnostics' && init.body instanceof Blob) {
			expect(headers.get('Content-Type')).toBe('application/gzip');
			await service.upload(id, token, kind, init.body);
			return new Response(null, { status: 204 });
		}
		return new Response(null, { status: 404 });
	}) as typeof fetch;
	const transport = createRemoteReportTransport(
		'https://reporting.invalid',
		async (manifest, key) => {
			const opened = service.create(service.recorderKey, key, manifest, 10);
			return { id: opened.id, token: opened.grant };
		},
		request
	);
	const submission = new ReportSubmission(transport, {
		text: {
			title: 'Test',
			description: 'Bug',
			reproduction: 'Open clip',
			expected: 'Plays',
			actual: 'Pauses'
		},
		diagnostics: new Blob(['diagnostic'], { type: 'application/gzip' }),
		evidenceWarnings: []
	});
	expect(requests).toEqual([]);
	expect(await submission.submit(() => {})).toMatchObject({ state: 'complete' });
	expect(requests).toEqual(['GET ', 'PUT artifacts', 'POST finalize']);
	expect(service.createdIssues()).toHaveLength(1);
});

it('rejects invalid destinations and opaque identifiers before sending a grant', async () => {
	const create = async () => ({ id: 'one', token: 'private-grant' });
	const invalid = [
		'http://reporting.invalid',
		'https://user@reporting.invalid',
		'https://reporting.invalid/base',
		'https://reporting.invalid/?grant=secret'
	];
	for (const origin of invalid)
		expect(() => createRemoteReportTransport(origin, create)).toThrow('HTTPS service origin');
	let attempted = false;
	const request = (async () => {
		attempted = true;
		return new Response();
	}) as typeof fetch;
	const transport = createRemoteReportTransport('https://reporting.invalid', create, request);
	await expect(
		transport.status({ id: '../another', token: 'private-grant' }, new AbortController().signal)
	).rejects.toThrow('Invalid report identifier');
	expect(attempted).toBe(false);
});

it('bounds service error handling without exposing status response bodies', async () => {
	const create = async () => ({ id: 'one', token: 'private-grant' });
	const document = (async () => new Response('x'.repeat(8_193))) as typeof fetch;
	const grant = await create();
	const signal = new AbortController().signal;
	await expect(
		createRemoteReportTransport('https://reporting.invalid', create, document).status(grant, signal)
	).rejects.toThrow('Report status was too large');
	const unauthorized = (async () =>
		new Response('private error detail', { status: 403 })) as typeof fetch;
	await expect(
		createRemoteReportTransport('https://reporting.invalid', create, unauthorized).status(
			grant,
			signal
		)
	).rejects.toThrow('The reporting service did not accept the request.');
});
