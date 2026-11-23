import { createHash, randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';

const schema = JSON.parse(
	readFileSync(new URL('../../schemas/reporting/report-v1.schema.json', import.meta.url), 'utf8')
) as typeof import('../../schemas/reporting/report-v1.schema.json');

type ArtifactKind = 'diagnostics' | 'screen';
type Artifact = { type: string; bytes: number };
const textFields = ['title', 'description', 'reproduction', 'expected', 'actual'] as const;
type ReportManifest = Record<(typeof textFields)[number], string> & {
	version: 1;
	evidenceWarnings: string[];
	artifacts: { diagnostics: Artifact; screen?: Artifact };
};
type Upload = { body: Blob; digest: string };
type Report = {
	id: string;
	manifest: ReportManifest;
	request: string;
	expires: number;
	retentionMs: number;
	deleteAt: number | null;
	grant: string;
	uploads: Map<ArtifactKind, Upload>;
	issueUrl: string | null;
	cancelled: boolean;
};

class ReportError extends Error {}

function requireReport(condition: unknown, reason: string): asserts condition {
	if (!condition) throw new ReportError(reason);
}

function closedObject(value: unknown, allowed: readonly string[]): Record<string, unknown> {
	requireReport(
		value !== null && typeof value === 'object' && !Array.isArray(value),
		'invalid-manifest'
	);
	const fields = value as Record<string, unknown>;
	requireReport(
		Object.keys(fields).every((key) => allowed.includes(key)),
		'invalid-manifest'
	);
	return fields;
}

function artifact(value: unknown, kind: ArtifactKind): Artifact {
	const fields = closedObject(value, ['type', 'bytes']);
	const { type, bytes } = fields;
	const rules = schema.$defs[kind].properties;
	requireReport(typeof type === 'string' && rules.type.enum.includes(type), 'invalid-manifest');
	requireReport(
		typeof bytes === 'number' &&
			Number.isSafeInteger(bytes) &&
			bytes >= rules.bytes.minimum &&
			bytes <= rules.bytes.maximum,
		'invalid-manifest'
	);
	return { type, bytes };
}

function manifest(value: unknown): ReportManifest {
	const rules = schema.properties;
	const fields = closedObject(value, Object.keys(rules));
	requireReport(fields.version === rules.version.const, 'invalid-manifest');
	const text = {} as Record<(typeof textFields)[number], string>;
	for (const name of textFields) {
		const entry = fields[name];
		requireReport(
			typeof entry === 'string' &&
				entry.trim().length > 0 &&
				[...entry].length <= rules[name].maxLength,
			'invalid-manifest'
		);
		text[name] = entry;
	}
	const warnings = fields.evidenceWarnings;
	requireReport(
		Array.isArray(warnings) &&
			warnings.length <= rules.evidenceWarnings.maxItems &&
			new Set(warnings).size === warnings.length &&
			warnings.every((warning) => rules.evidenceWarnings.items.enum.includes(warning)),
		'invalid-manifest'
	);
	const artifacts = closedObject(fields.artifacts, ['diagnostics', 'screen']);
	return {
		...text,
		version: 1,
		evidenceWarnings: [...warnings].sort(),
		artifacts: {
			diagnostics: artifact(artifacts.diagnostics, 'diagnostics'),
			...(Object.hasOwn(artifacts, 'screen')
				? { screen: artifact(artifacts.screen, 'screen') }
				: {})
		}
	};
}

// This fixture makes no network requests and owns only the data supplied by one test.
export function reportingService(now: () => number, privateCanaries: readonly string[]) {
	const reports = new Map<string, Report>();
	const idempotency = new Map<string, Report>();
	const issueBodies: string[] = [];
	const recorderKey = randomUUID();
	const maintainerKey = randomUUID();
	const day = 86_400_000;

	function find(id: string): Report {
		const report = reports.get(id);
		requireReport(report, 'unknown-report');
		expire(report);
		return report;
	}

	function expire(report: Report): void {
		if (report.deleteAt !== null ? now() >= report.deleteAt : now() >= report.expires) {
			report.uploads.clear();
		}
	}

	function authorize(id: string, grant: string): Report {
		const report = find(id);
		requireReport(report.grant === grant, 'unauthorized');
		requireReport(now() < report.expires, 'expired-grant');
		return report;
	}

	function expectedUpload(id: string, grant: string, kind: ArtifactKind, type: string): Artifact {
		const report = authorize(id, grant);
		requireReport(!report.cancelled && !report.issueUrl, 'report-terminal');
		const expected = report.manifest.artifacts[kind];
		requireReport(expected?.type === type, 'artifact-type-mismatch');
		return expected;
	}

	function state(report: Report) {
		if (report.cancelled) return { state: 'cancelled' } as const;
		if (report.issueUrl) return { state: 'complete', issueUrl: report.issueUrl } as const;
		return { state: 'pending' } as const;
	}

	return {
		recorderKey,
		maintainerKey,
		createdIssues: () => [...issueBodies],
		sweep: () => reports.forEach(expire),
		create(key: string, idempotencyKey: string, value: unknown, retentionDays: number) {
			requireReport(key === recorderKey, 'unauthorized');
			requireReport(/^[a-zA-Z0-9-]{1,64}$/.test(idempotencyKey), 'invalid-idempotency-key');
			requireReport(
				Number.isInteger(retentionDays) && retentionDays >= 1 && retentionDays <= 30,
				'invalid-retention'
			);
			const validated = manifest(value);
			const request = JSON.stringify([validated, retentionDays]);
			let report = idempotency.get(idempotencyKey);
			if (report) requireReport(report.request === request, 'idempotency-conflict');
			else {
				report = {
					id: randomUUID(),
					manifest: validated,
					request,
					expires: now() + 300_000,
					grant: randomUUID(),
					retentionMs: retentionDays * day,
					deleteAt: null,
					uploads: new Map(),
					issueUrl: null,
					cancelled: false
				};
				reports.set(report.id, report);
				idempotency.set(idempotencyKey, report);
			}
			return { id: report.id, grant: report.grant, expires: report.expires };
		},
		async upload(id: string, grant: string, kind: ArtifactKind, body: Blob) {
			const expected = expectedUpload(id, grant, kind, body.type);
			const report = authorize(id, grant);
			requireReport(body.size === expected.bytes, 'artifact-size-mismatch');
			const digest = createHash('sha256')
				.update(Buffer.from(await body.arrayBuffer()))
				.digest('hex');
			authorize(id, grant);
			requireReport(!report.cancelled && !report.issueUrl, 'report-terminal');
			const previous = report.uploads.get(kind);
			requireReport(!previous || previous.digest === digest, 'artifact-conflict');
			report.uploads.set(kind, { body, digest });
		},
		finalize(id: string, grant: string) {
			const report = authorize(id, grant);
			if (report.issueUrl || report.cancelled) return state(report);
			requireReport(
				Object.keys(report.manifest.artifacts).every((kind) =>
					report.uploads.has(kind as ArtifactKind)
				),
				'incomplete-uploads'
			);
			const publicText = [
				`Report ${report.id}`,
				...textFields.map((field) => report.manifest[field]),
				...report.manifest.evidenceWarnings
			].join('\n');
			requireReport(
				!privateCanaries.some((value) => value && publicText.includes(value)),
				'private-public-output'
			);
			issueBodies.push(publicText);
			report.issueUrl = `https://issues.invalid/keeppeek/${issueBodies.length}`;
			report.deleteAt = now() + report.retentionMs;
			return state(report);
		},
		expectedUpload,
		status: (id: string, grant: string) => state(authorize(id, grant)),
		cancel(id: string, grant: string) {
			const report = authorize(id, grant);
			requireReport(!report.issueUrl, 'report-terminal');
			report.cancelled = true;
			report.uploads.clear();
			return state(report);
		},
		read(id: string, key: string, kind: ArtifactKind) {
			requireReport(key === maintainerKey, 'unauthorized');
			const report = find(id);
			requireReport(report.issueUrl, 'report-pending');
			return report.uploads.get(kind)?.body ?? null;
		}
	};
}

async function receiveArtifact(request: Request, expected: Artifact): Promise<Blob> {
	const length = request.headers.get('Content-Length');
	requireReport(length === null || length === String(expected.bytes), 'artifact-size-mismatch');
	const reader = request.body?.getReader();
	requireReport(reader, 'artifact-size-mismatch');
	const parts: Uint8Array<ArrayBuffer>[] = [];
	let count = 0;
	try {
		while (true) {
			const chunk = await reader.read();
			if (chunk.done) break;
			count += chunk.value.byteLength;
			requireReport(count <= expected.bytes, 'artifact-size-mismatch');
			parts.push(new Uint8Array(chunk.value));
		}
		requireReport(count === expected.bytes, 'artifact-size-mismatch');
		request.signal.throwIfAborted();
		return new Blob(parts, { type: expected.type });
	} finally {
		await reader.cancel().catch(() => undefined);
		reader.releaseLock();
	}
}

function reportErrorStatus(code: string): number {
	switch (code) {
		case 'unauthorized':
			return 401;
		case 'unknown-report':
			return 404;
		case 'expired-grant':
			return 410;
		case 'artifact-size-mismatch':
		case 'manifest-too-large':
			return 413;
		case 'artifact-type-mismatch':
		case 'manifest-type-mismatch':
			return 415;
		case 'report-terminal':
		case 'incomplete-uploads':
		case 'artifact-conflict':
		case 'idempotency-conflict':
			return 409;
		default:
			return 400;
	}
}

const maxCreateBytes = 128 * 1024;

async function receiveCreate(request: Request): Promise<Record<string, unknown>> {
	requireReport(
		request.headers.get('Content-Type') === 'application/json',
		'manifest-type-mismatch'
	);
	const reader = request.body?.getReader();
	requireReport(reader, 'invalid-manifest');
	const decoder = new TextDecoder('utf-8', { fatal: true });
	let bytes = 0;
	let json = '';
	try {
		while (true) {
			const chunk = await reader.read();
			if (chunk.done) break;
			bytes += chunk.value.byteLength;
			requireReport(bytes <= maxCreateBytes, 'manifest-too-large');
			json += decoder.decode(chunk.value, { stream: true });
		}
		request.signal.throwIfAborted();
		return closedObject(JSON.parse(json + decoder.decode()), [
			'idempotencyKey',
			'manifest',
			'retentionDays'
		]);
	} finally {
		await reader.cancel().catch(() => undefined);
		reader.releaseLock();
	}
}

/** Deterministic wire fixture for the separate reporting service. */
export function reportingRequestHandler(
	service: ReturnType<typeof reportingService>,
	recorderOrigin: string
) {
	const cors = {
		'Access-Control-Allow-Origin': recorderOrigin,
		'Access-Control-Allow-Headers': 'Authorization, Content-Type',
		'Access-Control-Allow-Methods': 'GET, PUT, POST, DELETE, OPTIONS',
		'Cache-Control': 'no-store',
		Vary: 'Origin'
	};
	const empty = (status: number) => new Response(null, { status, headers: cors });

	return async (request: Request): Promise<Response> => {
		const path = new URL(request.url);
		const grant = /^Bearer (\S+)$/.exec(request.headers.get('Authorization') ?? '')?.[1];
		if (path.pathname === '/v1/reports' && !path.search) {
			const headers = { 'Cache-Control': 'no-store' };
			const reject = (status: number) => new Response(null, { status, headers });
			if (request.headers.has('Origin')) return reject(403);
			if (request.method !== 'POST') return reject(405);
			if (grant !== service.recorderKey) {
				await request.body?.cancel().catch(() => undefined);
				return reject(401);
			}
			try {
				const body = await receiveCreate(request);
				requireReport(
					typeof body.idempotencyKey === 'string' && typeof body.retentionDays === 'number',
					'invalid-manifest'
				);
				return Response.json(
					service.create(grant, body.idempotencyKey, body.manifest, body.retentionDays),
					{ headers }
				);
			} catch (error) {
				await request.body?.cancel().catch(() => undefined);
				return reject(error instanceof ReportError ? reportErrorStatus(error.message) : 400);
			}
		}
		if (request.headers.get('Origin') !== recorderOrigin)
			return new Response(null, { status: 403 });
		const match =
			/^\/v1\/reports\/([A-Za-z0-9_-]{1,128})(?:\/(finalize|artifacts\/(diagnostics|screen)))?$/.exec(
				path.pathname
			);
		if (!match || path.search) return empty(404);
		if (request.method === 'OPTIONS') return empty(204);
		const [, id, suffix, kind] = match;
		if (!grant) return empty(401);
		try {
			if (request.method === 'GET' && !suffix) {
				return Response.json(service.status(id, grant), { headers: cors });
			}
			if (request.method === 'POST' && suffix === 'finalize') {
				return Response.json(service.finalize(id, grant), { headers: cors });
			}
			if (request.method === 'DELETE' && !suffix) {
				service.cancel(id, grant);
				return empty(204);
			}
			if (request.method === 'PUT' && (kind === 'diagnostics' || kind === 'screen')) {
				const expected = service.expectedUpload(
					id,
					grant,
					kind,
					request.headers.get('Content-Type') ?? ''
				);
				const body = await receiveArtifact(request, expected);
				await service.upload(id, grant, kind, body);
				return empty(204);
			}
			return empty(405);
		} catch (error) {
			await request.body?.cancel().catch(() => undefined);
			return empty(error instanceof ReportError ? reportErrorStatus(error.message) : 400);
		}
	};
}
