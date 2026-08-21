import { describe, expect, it } from 'vitest';
import { firstRunStorageEvidence } from '$lib/first-run';
import type { DiskHealth } from '$lib/types';

const recordingDisk: DiskHealth = {
	name: 'recordings',
	kind: 'SSD',
	file_system: 'apfs',
	mount_point: '/mnt/keeppeek',
	total_bytes: 8_000_000_000_000,
	available_bytes: 7_900_000_000_000,
	used_bytes: 100_000_000_000,
	removable: false,
	stores_recordings: true
};

const rootDisk: DiskHealth = {
	...recordingDisk,
	name: 'system',
	mount_point: '/',
	stores_recordings: true
};

describe('first-run storage evidence', () => {
	it('does not treat matching disk capacity as write-permission proof', () => {
		expect(firstRunStorageEvidence('/mnt/keeppeek', [recordingDisk], null)).toEqual({
			path: '/mnt/keeppeek',
			diskName: 'recordings',
			mountPoint: '/mnt/keeppeek',
			availableBytes: 7_900_000_000_000,
			writeStatus: 'unavailable',
			detail:
				'KeepPeek does not expose a candidate storage write probe, so recording cannot be started from this screen.',
			canStartRecorder: false
		});
	});

	it('unlocks recording only after a successful write probe', () => {
		const evidence = firstRunStorageEvidence('/mnt/keeppeek', [recordingDisk], {
			writable: true,
			detail: 'A temporary file was written and removed.'
		});

		expect(evidence.writeStatus).toBe('verified');
		expect(evidence.canStartRecorder).toBe(true);
	});

	it('uses the most-specific volume containing the candidate path', () => {
		const candidateDisk = {
			...recordingDisk,
			name: 'candidate',
			mount_point: '/mnt/onboarding',
			available_bytes: 6_500_000_000_000,
			stores_recordings: false
		};
		const evidence = firstRunStorageEvidence(
			'/mnt/onboarding/keeppeek',
			[rootDisk, recordingDisk, candidateDisk],
			null
		);

		expect(evidence).toMatchObject({
			diskName: 'candidate',
			mountPoint: '/mnt/onboarding',
			availableBytes: 6_500_000_000_000
		});
	});

	it('preserves a failed probe as an explicit blocker', () => {
		const evidence = firstRunStorageEvidence('/mnt/keeppeek', [], {
			writable: false,
			detail: 'Permission denied.'
		});

		expect(evidence).toMatchObject({
			diskName: null,
			availableBytes: null,
			writeStatus: 'failed',
			detail: 'Permission denied.',
			canStartRecorder: false
		});
	});
});
