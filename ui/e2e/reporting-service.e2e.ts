import { expect, test } from '@playwright/test';
import { gzipSync } from 'node:zlib';
import { reportingService } from './reporting-service';

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
