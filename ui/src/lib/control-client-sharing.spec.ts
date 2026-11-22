import { create } from '@bufbuild/protobuf';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { SharingControlClient, canonicalShareOrigin } from './control-client-sharing';
import { ShareResultSchema, type Ok, type Request } from './proto/webrtc_pb';
import {
	shareEvidenceFixture,
	sharePolicyFixture,
	shareGrantFixture,
	shareIssuedFixture,
	shareListFixture,
	sharingFixtureNow,
	sharingFixtureOrigin,
	sharingFixtureRequestId,
	sharingFixtureToken
} from './sharing.fixture';

function result(value: Parameters<typeof create<typeof ShareResultSchema>>[1]): Ok['result'] {
	return { case: 'shareResult', value: create(ShareResultSchema, value) };
}
function input() {
	const evidence = shareEvidenceFixture();
	return {
		requestId: sharingFixtureRequestId,
		exportJobId: evidence.exportJobId,
		artifactId: evidence.artifactId,
		expectedSha256: evidence.sha256,
		expiresAtMs: BigInt(sharingFixtureNow + 3_600_000),
		displayName: 'Front step visit',
		note: '',
		allowStream: true,
		allowDownload: false,
		maxViews: 2
	};
}
afterEach(() => vi.restoreAllMocks());

describe('sharing control client', () => {
	it('sends policy and immutable ready-export preview actions through the sharing contract', async () => {
		const replies = [
			result({ result: { case: 'policy', value: sharePolicyFixture() } }),
			result({ result: { case: 'evidence', value: shareEvidenceFixture() } })
		];
		const sent: Request['command'][] = [];
		const client = new SharingControlClient(async (command) => {
			sent.push(command);
			return replies.shift()!;
		});
		expect((await client.getPolicy()).revision).toBe('policy-1');
		expect((await client.previewExport('export-ready-132')).artifactId).toBe('artifact-132');
		expect(
			sent.map((command) => command.case === 'shareCommand' && command.value.action.case)
		).toEqual(['getPolicy', 'previewExport']);
	});

	it('requires an exact canonical HTTPS origin and keeps revision-checked policy writes', async () => {
		const send = vi
			.fn()
			.mockResolvedValue(result({ result: { case: 'policy', value: sharePolicyFixture() } }));
		const client = new SharingControlClient(send);
		for (const origin of [
			'http://recorder.example',
			'https://recorder.example/',
			'https://u:p@recorder.example',
			'https://recorder.example/x',
			'https://recorder.example#x'
		]) {
			expect(() => canonicalShareOrigin(origin)).toThrow();
		}
		expect(canonicalShareOrigin(sharingFixtureOrigin)).toBe(sharingFixtureOrigin);
		await client.configurePolicy({ enabled: true, publicOrigin: sharingFixtureOrigin }, 'policy-1');
		const command = send.mock.calls[0][0];
		expect(command.value.action).toMatchObject({
			case: 'configurePolicy',
			value: { expectedRevision: 'policy-1', settings: { enabled: true } }
		});
	});

	it('bounds live grant pages and never obtains a bearer URL through list', async () => {
		const send = vi
			.fn()
			.mockResolvedValue(result({ result: { case: 'grants', value: shareListFixture() } }));
		const client = new SharingControlClient(send);
		const listed = await client.list();
		expect(listed.grants[0].grantId).toBe(sharingFixtureRequestId);
		expect('url' in listed.grants[0]).toBe(false);
		expect(send.mock.calls[0][0].value.action.value).toMatchObject({
			pageSize: 50,
			allCreators: false
		});
		await expect(client.list({ pageSize: 101 })).rejects.toThrow();
		expect(send).toHaveBeenCalledTimes(1);
	});

	it('creates only the exact previewed artifact and returns the one-time secret without caching it', async () => {
		vi.spyOn(Date, 'now').mockReturnValue(sharingFixtureNow);
		const send = vi
			.fn()
			.mockResolvedValue(result({ result: { case: 'created', value: shareIssuedFixture() } }));
		const client = new SharingControlClient(send);
		const issued = await client.create(input(), sharingFixtureOrigin);
		expect(issued.url).toBe(`${sharingFixtureOrigin}/share#${sharingFixtureToken}`);
		expect(send.mock.calls[0][0].value.action).toMatchObject({ case: 'create', value: input() });
		expect(Object.values(client)).not.toContain(issued);
	});

	it('rejects mismatched evidence or an unexpected disclosure origin with fixed errors', async () => {
		vi.spyOn(Date, 'now').mockReturnValue(sharingFixtureNow);
		for (const altered of [
			{ ...shareIssuedFixture(), url: `https://other.example/share#${sharingFixtureToken}` },
			{
				...shareIssuedFixture(),
				grant: {
					...shareGrantFixture(),
					evidence: { ...shareEvidenceFixture(), artifactId: 'other' }
				}
			}
		]) {
			const client = new SharingControlClient(async () =>
				result({ result: { case: 'created', value: altered } })
			);
			await expect(client.create(input(), sharingFixtureOrigin)).rejects.toThrow(
				'Server returned an invalid sharing response.'
			);
		}
	});

	it('rejects invalid expiry, quotas, and mode choices before sending creation', async () => {
		vi.spyOn(Date, 'now').mockReturnValue(sharingFixtureNow);
		const send = vi.fn();
		const client = new SharingControlClient(send);
		for (const invalid of [
			{ maxViews: 0 },
			{ maxDownloads: 4_294_967_296 },
			{ expiresAtMs: BigInt(sharingFixtureNow) },
			{ expiresAtMs: BigInt(sharingFixtureNow + 86_400_001) },
			{ allowStream: false, allowDownload: false }
		])
			await expect(
				client.create({ ...input(), ...invalid }, sharingFixtureOrigin)
			).rejects.toThrow();
		expect(send).not.toHaveBeenCalled();
	});

	it('keeps exact revisions and explicit artifact/source selection for revocation', async () => {
		const sent: Request['command'][] = [];
		const client = new SharingControlClient(async (command) => {
			sent.push(command);
			return command.case === 'shareCommand' && command.value.action.case === 'revoke'
				? result({ result: { case: 'grant', value: shareGrantFixture() } })
				: result({ result: { case: 'revokedEvidence', value: { revokedCount: 2 } } });
		});
		await client.revoke(shareGrantFixture(), 'No longer needed');
		await client.revokeEvidence({ artifactId: 'artifact-132' }, 'No longer needed');
		await client.revokeEvidence(
			{ sourceIdentity: shareEvidenceFixture().sourceIdentity },
			'Camera removed'
		);
		expect(sent[0]).toMatchObject({
			case: 'shareCommand',
			value: { action: { case: 'revoke', value: { expectedRevision: 1n } } }
		});
		expect(sent[1]).toMatchObject({
			value: { action: { value: { evidence: { case: 'artifactId', value: 'artifact-132' } } } }
		});
		expect(sent[2]).toMatchObject({
			value: {
				action: {
					value: {
						evidence: { case: 'sourceIdentity', value: shareEvidenceFixture().sourceIdentity }
					}
				}
			}
		});
	});
});
