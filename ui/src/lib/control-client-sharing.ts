import { create, type MessageInitShape } from '@bufbuild/protobuf';
import {
	ShareCommandSchema,
	CreateShareGrantSchema,
	SharePolicySettingsSchema,
	type ShareResult,
	type SharePolicy,
	type ShareEvidence,
	type ShareGrant,
	type ShareGrantList,
	type IssuedShareGrant,
	type Ok,
	type Request
} from './proto/webrtc_pb';

export const sharingCapability = 'keeppeek.guest-review.v1';
export type ShareCreateInput = MessageInitShape<typeof CreateShareGrantSchema>;
export type SharePolicyInput = MessageInitShape<typeof SharePolicySettingsSchema>;
type Send = (command: Request['command']) => Promise<Ok['result']>;
const responseError = 'Server returned an invalid sharing response.';
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const checksum = /^[0-9a-f]{64}$/i;
const maxDateMs = 8_640_000_000_000_000n;

export class SharingControlClient {
	constructor(private readonly send: Send) {}

	async getPolicy(): Promise<SharePolicy> {
		const result = await this.request({ case: 'getPolicy', value: {} });
		if (result.case !== 'policy') throw new Error(responseError);
		return validPolicy(result.value);
	}

	async configurePolicy(
		settings: SharePolicyInput,
		expectedRevision: string
	): Promise<SharePolicy> {
		if (!text(expectedRevision, 256)) throw new Error('Reload the sharing policy before saving.');
		if (settings.publicOrigin !== undefined) canonicalShareOrigin(settings.publicOrigin);
		if (settings.enabled && !settings.publicOrigin)
			throw new Error('Enter a reachable HTTPS origin.');
		const result = await this.request({
			case: 'configurePolicy',
			value: { settings, expectedRevision }
		});
		if (result.case !== 'policy') throw new Error(responseError);
		return validPolicy(result.value);
	}

	async previewExport(exportJobId: string): Promise<ShareEvidence> {
		if (!text(exportJobId, 64)) throw new Error('Select a completed export.');
		const result = await this.request({ case: 'previewExport', value: { exportJobId } });
		if (result.case !== 'evidence' || result.value.exportJobId !== exportJobId)
			throw new Error(responseError);
		return validEvidence(result.value);
	}

	async create(input: ShareCreateInput, expectedOrigin: string): Promise<IssuedShareGrant> {
		canonicalShareOrigin(expectedOrigin);
		const intent = create(CreateShareGrantSchema, input);
		validCreate(intent);
		const result = await this.request({ case: 'create', value: intent });
		if (result.case !== 'created') throw new Error(responseError);
		const issued = result.value;
		const grant = issued.grant && validGrant(issued.grant);
		if (
			!grant ||
			grant.grantId !== intent.requestId ||
			grant.evidence?.exportJobId !== intent.exportJobId ||
			grant.evidence.artifactId !== intent.artifactId ||
			grant.evidence.sha256 !== intent.expectedSha256 ||
			grant.expiresAtMs !== intent.expiresAtMs ||
			grant.allowStream !== intent.allowStream ||
			grant.allowDownload !== intent.allowDownload ||
			grant.maxViews !== intent.maxViews ||
			grant.maxDownloads !== intent.maxDownloads
		)
			throw new Error(responseError);
		let url: URL;
		try {
			url = new URL(issued.url);
		} catch {
			throw new Error(responseError);
		}
		if (
			url.origin !== expectedOrigin ||
			url.pathname !== '/share' ||
			url.search ||
			url.username ||
			url.password ||
			!/^#[A-Za-z0-9_-]{43}$/.test(url.hash) ||
			url.href !== issued.url
		)
			throw new Error(responseError);
		return issued;
	}

	async list(
		options: { pageSize?: number; afterGrantId?: string; allCreators?: boolean } = {}
	): Promise<ShareGrantList> {
		const pageSize = options.pageSize ?? 50;
		if (
			!Number.isInteger(pageSize) ||
			pageSize < 1 ||
			pageSize > 100 ||
			(options.afterGrantId !== undefined && !uuid.test(options.afterGrantId))
		)
			throw new Error('Select a valid guest link page.');
		const result = await this.request({
			case: 'list',
			value: {
				pageSize,
				afterGrantId: options.afterGrantId,
				allCreators: options.allCreators ?? false
			}
		});
		if (
			result.case !== 'grants' ||
			result.value.grants.length > pageSize ||
			(result.value.nextAfterGrantId !== undefined && !uuid.test(result.value.nextAfterGrantId))
		)
			throw new Error(responseError);
		if (
			new Set(result.value.grants.map((grant) => grant.grantId)).size !== result.value.grants.length
		)
			throw new Error(responseError);
		result.value.grants.forEach(validGrant);
		return result.value;
	}

	async revoke(
		grant: Pick<ShareGrant, 'grantId' | 'revision'>,
		reason: string
	): Promise<ShareGrant> {
		if (!uuid.test(grant.grantId) || grant.revision < 1n || !text(reason, 256))
			throw new Error('Reload the link and enter a revocation reason.');
		const result = await this.request({
			case: 'revoke',
			value: { grantId: grant.grantId, expectedRevision: grant.revision, reason }
		});
		if (result.case !== 'grant' || result.value.grantId !== grant.grantId)
			throw new Error(responseError);
		return validGrant(result.value);
	}

	async revokeEvidence(
		selection: { artifactId: string } | { sourceIdentity: string },
		reason: string
	): Promise<number> {
		if (!text(reason, 256)) throw new Error('Enter a revocation reason.');
		const evidence =
			'artifactId' in selection
				? { case: 'artifactId' as const, value: selection.artifactId }
				: { case: 'sourceIdentity' as const, value: selection.sourceIdentity };
		if (
			!text(evidence.value, 128) ||
			(evidence.case === 'sourceIdentity' && !uuid.test(evidence.value))
		)
			throw new Error('Reload the evidence before revoking its links.');
		const result = await this.request({ case: 'revokeEvidence', value: { evidence, reason } });
		if (result.case !== 'revokedEvidence' || result.value.revokedCount > 256)
			throw new Error(responseError);
		return result.value.revokedCount;
	}

	private async request(
		action: MessageInitShape<typeof ShareCommandSchema>['action']
	): Promise<ShareResult['result']> {
		const result = await this.send({
			case: 'shareCommand',
			value: create(ShareCommandSchema, { action })
		});
		if (result.case !== 'shareResult' || result.value.result.case === undefined)
			throw new Error(responseError);
		return result.value.result;
	}
}

export function canonicalShareOrigin(value: string): string {
	let parsed: URL;
	try {
		parsed = new URL(value);
	} catch {
		throw new Error('Enter an HTTPS origin without a path or trailing slash.');
	}
	if (
		value.length > 512 ||
		parsed.protocol !== 'https:' ||
		parsed.origin !== value ||
		parsed.username ||
		parsed.password ||
		parsed.pathname !== '/' ||
		parsed.search ||
		parsed.hash
	)
		throw new Error('Enter an HTTPS origin without a path or trailing slash.');
	return parsed.origin;
}

function text(value: string, limit: number, empty = false): boolean {
	return (
		(empty || value.trim().length > 0) &&
		new TextEncoder().encode(value).byteLength <= limit &&
		!/\p{Cc}/u.test(value)
	);
}
function date(value: bigint): boolean {
	return value >= 0n && value <= maxDateMs;
}
function quota(value: number | undefined): boolean {
	return value === undefined || (Number.isInteger(value) && value > 0 && value <= 4_294_967_295);
}
function validPolicy(value: SharePolicy): SharePolicy {
	if (
		!value.settings ||
		!text(value.revision, 256) ||
		(value.unavailableReason !== undefined && !text(value.unavailableReason, 512, true))
	)
		throw new Error(responseError);
	if (value.settings.publicOrigin !== undefined) {
		try {
			canonicalShareOrigin(value.settings.publicOrigin);
		} catch {
			throw new Error(responseError);
		}
	}
	if (value.settings.enabled && !value.settings.publicOrigin) throw new Error(responseError);
	return value;
}
function validEvidence(value: ShareEvidence): ShareEvidence {
	if (
		!text(value.exportJobId, 64) ||
		!text(value.artifactId, 128) ||
		!checksum.test(value.sha256) ||
		!text(value.sourceId, 256) ||
		!uuid.test(value.sourceIdentity) ||
		!date(value.startMs) ||
		!date(value.endMs) ||
		value.endMs <= value.startMs ||
		value.endMs - value.startMs > 120_000n ||
		value.bytes <= 0n ||
		value.bytes > 536_870_912n ||
		!date(value.availableUntilMs)
	)
		throw new Error(responseError);
	return value;
}
function validGrant(value: ShareGrant): ShareGrant {
	if (
		!uuid.test(value.grantId) ||
		value.revision < 1n ||
		!text(value.displayName, 128) ||
		!text(value.creatorId, 256) ||
		(value.lastAccessAtMs !== undefined && !date(value.lastAccessAtMs)) ||
		(value.revokeReason !== undefined && !text(value.revokeReason, 256)) ||
		!text(value.note, 512, true) ||
		!date(value.createdAtMs) ||
		!date(value.expiresAtMs) ||
		!value.evidence ||
		!quota(value.maxViews) ||
		!quota(value.maxDownloads)
	)
		throw new Error(responseError);
	validEvidence(value.evidence);
	return value;
}
function validCreate(value: ReturnType<typeof create<typeof CreateShareGrantSchema>>): void {
	const now = BigInt(Date.now());
	if (
		!uuid.test(value.requestId) ||
		!text(value.exportJobId, 64) ||
		!text(value.artifactId, 128) ||
		!checksum.test(value.expectedSha256) ||
		!text(value.displayName, 128) ||
		!text(value.note, 512, true) ||
		!date(value.expiresAtMs) ||
		value.expiresAtMs <= now ||
		value.expiresAtMs - now > 86_400_000n ||
		(!value.allowStream && !value.allowDownload) ||
		!quota(value.maxViews) ||
		!quota(value.maxDownloads)
	)
		throw new Error(
			'Review the name, expiry, permissions, and positive limits before creating a link.'
		);
}
