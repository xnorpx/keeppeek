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
			const report = authorize(id, grant);
			requireReport(!report.cancelled && !report.issueUrl, 'report-terminal');
			const expected = report.manifest.artifacts[kind];
			requireReport(expected?.type === body.type, 'artifact-type-mismatch');
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
