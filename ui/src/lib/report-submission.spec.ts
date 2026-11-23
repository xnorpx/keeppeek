import { expect, it } from 'vitest';
import { reportingService } from '../../e2e/reporting-service';
import { ReportSubmission, type ReportTransport, type ReportGrant } from './report-submission';

function setup() {
	const service = reportingService(() => 0, []);
	const diagnostics = new Blob(['diagnostics'], { type: 'application/gzip' });
	const screen = new Blob(['private screen'], { type: 'video/webm' });
	const text = {
		title: 'Example issue',
		description: 'Reproduction happened in the fixture.',
		reproduction: 'Load the page.',
		expected: 'Render the image.',
		actual: 'The image is missing.'
	};
	const keys: string[] = [];
	const grants: ReportGrant[] = [];
	const transport: ReportTransport = {
		async create(manifest, key) {
			keys.push(key);
			const opened = service.create(service.recorderKey, key, manifest, 10);
			const grant = { id: opened.id, token: opened.grant };
			grants.push(grant);
			return grant;
		},
		status: async ({ id, token }) => service.status(id, token),
		async upload({ id, token }, kind, artifact, _signal, bytes) {
			bytes(artifact.size);
			await service.upload(id, token, kind, artifact);
		},
		finalize: async ({ id, token }) => service.finalize(id, token),
		async cancel({ id, token }) {
			service.cancel(id, token);
		}
	};
	return { service, transport, text, diagnostics, screen, grants, keys };
}

it('waits for confirmation and reuses the key after an ambiguous finalization', async () => {
	const { service, transport, text, diagnostics, screen, grants, keys } = setup();
	const finalize = transport.finalize;
	transport.finalize = async (grant, signal) => {
		await finalize(grant, signal);
		throw new Error('Response lost');
	};
	const submission = new ReportSubmission(transport, {
		text,
		diagnostics,
		screen,
		evidenceWarnings: []
	});
	expect(keys).toHaveLength(0);
	const progress: number[] = [];
	const first = submission.submit(({ uploaded }) => progress.push(uploaded));
	expect(submission.submit(() => {})).toBe(first);
	await expect(first).rejects.toThrow('Response lost');
	expect(progress.at(-1)).toBe(diagnostics.size + screen.size);
	const result = await submission.submit(() => {});
	expect(result).toMatchObject({ state: 'complete' });
	expect(new Set(keys).size).toBe(1);
	expect(grants).toHaveLength(2);
	expect(grants[0]).toEqual(grants[1]);
	expect(service.createdIssues()).toHaveLength(1);
	expect(await submission.submit(() => {})).toEqual(result);
	expect(keys).toHaveLength(2);
});

it('cancels accepted staging after an upload fails and leaves the caller with its local package', async () => {
	const { service, transport, text, diagnostics, grants } = setup();
	const upload = transport.upload;
	transport.upload = async (...args) => {
		await upload(...args);
		throw new Error('Connection lost during upload');
	};
	const submission = new ReportSubmission(transport, { text, diagnostics, evidenceWarnings: [] });
	await expect(submission.submit(() => {})).rejects.toThrow('Connection lost during upload');
	await submission.cancel();
	const [{ id, token }] = grants;
	expect(service.status(id, token)).toEqual({ state: 'cancelled' });
	expect(await diagnostics.text()).toBe('diagnostics');
	expect(service.createdIssues()).toHaveLength(0);
});

it('refuses an invalid package before the service is called', () => {
	const { transport, text, keys } = setup();
	expect(
		() =>
			new ReportSubmission(transport, {
				text,
				diagnostics: new Blob(['bad'], { type: 'text/plain' }),
				evidenceWarnings: []
			})
	).toThrow('unsupported or oversized artifact');
	expect(keys).toHaveLength(0);
});

it('checks the original submission without uploading again when cancellation races finalization', async () => {
	const { service, transport, text, diagnostics, keys } = setup();
	const finalize = transport.finalize;
	let notifyCreated!: () => void;
	const created = new Promise<void>((resolve) => (notifyCreated = resolve));
	transport.finalize = async (grant, signal) => {
		await finalize(grant, signal);
		notifyCreated();
		return new Promise((_resolve, reject) => {
			signal.addEventListener('abort', () => reject(new Error('Request cancelled')), {
				once: true
			});
		});
	};
	const submission = new ReportSubmission(transport, { text, diagnostics, evidenceWarnings: [] });
	const sent = submission.submit(() => {});
	const settled = expect(sent).rejects.toThrow('Request cancelled');
	await created;
	expect(await submission.cancel()).toBe(false);
	await settled;
	transport.upload = async () => {
		throw new Error('Status reconciliation must not upload');
	};
	expect(await submission.check()).toMatchObject({ state: 'complete' });
	expect(keys).toHaveLength(1);
	expect(service.createdIssues()).toHaveLength(1);
});

it('preserves the original grant if the service cannot confirm cancellation', async () => {
	const { service, transport, text, diagnostics, keys } = setup();
	transport.upload = async () => {
		throw new Error('Upload failed');
	};
	transport.cancel = async () => {
		throw new Error('Cancellation unconfirmed');
	};
	const submission = new ReportSubmission(transport, { text, diagnostics, evidenceWarnings: [] });
	await expect(submission.submit(() => {})).rejects.toThrow('Upload failed');
	expect(await submission.cancel()).toBe(false);
	expect(await submission.check()).toEqual({ state: 'pending' });
	expect(keys).toHaveLength(1);
	expect(service.createdIssues()).toHaveLength(0);
});

it('carries safe diagnostics warnings into the submitted issue manifest', async () => {
	const { service, transport, text, diagnostics } = setup();
	const submission = new ReportSubmission(transport, {
		text,
		diagnostics,
		evidenceWarnings: ['diagnostics-logging-settings-unavailable']
	});
	await expect(submission.submit(() => {})).resolves.toMatchObject({ state: 'complete' });
	expect(service.createdIssues()[0]).toContain('diagnostics-logging-settings-unavailable');
});

it('rejects a private or unknown evidence warning before contacting the service', () => {
	const { transport, text, diagnostics, keys } = setup();
	expect(
		() =>
			new ReportSubmission(transport, {
				text,
				diagnostics,
				evidenceWarnings: ['unrecognized-private-canary']
			})
	).toThrow('unrecognized evidence warning');
	expect(keys).toHaveLength(0);
});
