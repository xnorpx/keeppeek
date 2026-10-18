import { create } from '@bufbuild/protobuf';
import {
	ShareEvidenceSchema,
	SharePolicySchema,
	ShareGrantSchema,
	IssuedShareGrantSchema,
	ShareGrantStatus,
	ShareGrantListSchema
} from './proto/webrtc_pb';

export const sharingFixtureNow = Date.UTC(2026, 9, 18, 6);
export const sharingFixtureOrigin = 'https://recorder.example';
export const sharingFixtureToken = 'A'.repeat(43);
export const sharingFixtureRequestId = '00000000-0000-4000-8000-000000000132';
export const shareEvidenceFixture = () =>
	create(ShareEvidenceSchema, {
		exportJobId: 'export-ready-132',
		artifactId: 'artifact-132',
		sha256: 'a'.repeat(64),
		sourceId: 'front-step',
		sourceIdentity: '00000000-0000-4000-8000-000000000133',
		cameraName: 'Front step',
		startMs: BigInt(Date.UTC(2026, 9, 17, 6, 11, 48)),
		endMs: BigInt(Date.UTC(2026, 9, 17, 6, 12, 48)),
		bytes: 24_600_000n,
		availableUntilMs: BigInt(sharingFixtureNow + 7_200_000),
		hasGaps: false
	});
export const sharePolicyFixture = () =>
	create(SharePolicySchema, {
		settings: { enabled: true, publicOrigin: sharingFixtureOrigin },
		revision: 'policy-1',
		available: true
	});
export const shareGrantFixture = () =>
	create(ShareGrantSchema, {
		grantId: sharingFixtureRequestId,
		revision: 1n,
		creatorId: 'local-administrator',
		displayName: 'Front step visit',
		note: '',
		createdAtMs: BigInt(sharingFixtureNow),
		expiresAtMs: BigInt(sharingFixtureNow + 3_600_000),
		status: ShareGrantStatus.ACTIVE,
		allowStream: true,
		allowDownload: false,
		maxViews: 2,
		views: 0,
		downloads: 0,
		evidence: shareEvidenceFixture()
	});
export const shareIssuedFixture = () =>
	create(IssuedShareGrantSchema, {
		grant: shareGrantFixture(),
		url: `${sharingFixtureOrigin}/share#${sharingFixtureToken}`
	});
export const shareListFixture = () =>
	create(ShareGrantListSchema, { grants: [shareGrantFixture()] });
