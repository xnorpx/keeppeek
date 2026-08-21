import { describe, expect, it } from 'vitest';
import { cameraDiagnosisEvidence, rankHealthFindings } from '$lib/health-presentation';
import type { CameraHealth, HealthIssue } from '$lib/types';

function camera(update: Partial<CameraHealth>): CameraHealth {
	return {
		id: 'back-yard',
		ip: '192.0.2.10',
		name: 'Back Yard',
		manufacturer: 'Reolink',
		model: 'RLC-Test',
		firmware_version: null,
		backend: 'retina',
		transport: 'udp',
		state: 'offline',
		lifecycle: 'reconnecting',
		last_error: 'Connection refused',
		configured_profiles: [],
		streams: [],
		...update
	};
}

function issue(update: Partial<HealthIssue>): HealthIssue {
	return {
		severity: 'warning',
		scope: 'system',
		message: 'CPU is warm',
		...update
	};
}

describe('health presentation', () => {
	it('ranks a camera recording outage before a same-severity system finding', () => {
		const findings = rankHealthFindings({
			cameras: [camera({})],
			issues: [issue({}), issue({ scope: 'Back Yard', message: 'No stream report' })]
		});

		expect(findings.map((finding) => finding.issue.message)).toEqual([
			'No stream report',
			'CPU is warm'
		]);
		expect(findings[0].camera?.id).toBe('back-yard');
	});

	it('keeps critical server evidence ahead of a warning camera outage', () => {
		const findings = rankHealthFindings({
			cameras: [camera({})],
			issues: [
				issue({ scope: 'Back Yard', message: 'No stream report' }),
				issue({ severity: 'critical', scope: 'storage', message: 'Disk is full' })
			]
		});

		expect(findings[0].issue.message).toBe('Disk is full');
	});

	it('builds diagnosis from observed fields and leaves missing evidence unavailable', () => {
		const snapshot = {
			cameras: [
				camera({
					state: 'degraded',
					streams: [
						{
							type: 'video_main',
							updated_at_ms: 1_700_000_000_000,
							report_age_ms: 2_000,
							reconnects: 3,
							drops: 14,
							errors: 1
						}
					]
				}),
				camera({ id: 'front-door', name: 'Front Door', state: 'online' })
			],
			issues: [issue({ scope: 'back-yard', message: 'Frames are dropping' })]
		};

		expect(cameraDiagnosisEvidence(snapshot, 'back-yard')).toMatchObject({
			latestStreamReportAtMs: 1_700_000_000_000,
			reconnects: 3,
			drops: 14,
			errors: 1,
			recordingGapStartMs: null,
			retryEvidence: null,
			credentialProbeAvailable: false,
			canSuggestTcp: true,
			reportingNormally: 1,
			otherUnhealthyCameras: 0
		});
	});

	it('does not turn absent stream counters into observed zeroes', () => {
		const evidence = cameraDiagnosisEvidence(
			{ cameras: [camera({ transport: 'tcp' })], issues: [] },
			'back-yard'
		);

		expect(evidence).toMatchObject({
			latestStreamReportAtMs: null,
			reconnects: null,
			drops: null,
			errors: null,
			canSuggestTcp: false
		});
	});
});
