import { create } from '@bufbuild/protobuf';
import { timestampFromDate } from '@bufbuild/protobuf/wkt';
import { describe, expect, it } from 'vitest';
import { numeric, serverHealth } from './control-client-system';
import {
	CameraHealthSnapshotSchema,
	PreRecordingDiagnosticsSchema,
	PreRecordingReason,
	EventRecordingStream,
	EventSchema,
	HealthTotalsSnapshotSchema,
	HealthIssueSnapshotSchema,
	LoadHealthSnapshotSchema,
	MemoryHealthSnapshotSchema,
	ProcessHealthSnapshotSchema,
	RecordingDemandHealthSnapshotSchema,
	ServerHealthSnapshotSchema,
	StorageHealthSnapshotSchema,
	SystemHealthSnapshotSchema,
	WebRtcHealthSnapshotSchema
} from './proto/webrtc_pb';
import type { RecordingEvent } from './types';

const unusedEventMapper = (): RecordingEvent => {
	throw new Error('Unexpected operational event.');
};

describe('control client system mapping', () => {
	it('normalizes known and future health values without weakening the response contract', () => {
		const health = serverHealth(
			create(ServerHealthSnapshotSchema, {
				healthContractVersion: 1,
				totals: create(HealthTotalsSnapshotSchema),
				system: create(SystemHealthSnapshotSchema, {
					process: create(ProcessHealthSnapshotSchema),
					memory: create(MemoryHealthSnapshotSchema),
					load: create(LoadHealthSnapshotSchema)
				}),
				storage: create(StorageHealthSnapshotSchema, {
					demand: create(RecordingDemandHealthSnapshotSchema)
				}),
				webrtc: create(WebRtcHealthSnapshotSchema),
				cameras: [
					create(CameraHealthSnapshotSchema, {
						id: 'known',
						preRecording: create(PreRecordingDiagnosticsSchema, {
							enabled: true,
							active: false,
							selectedStream: EventRecordingStream.SUB,
							requestedMs: 30_000n,
							availableMs: 4_000n,
							retainedBytes: 1_048_576n,
							reason: PreRecordingReason.GLOBAL_PRESSURE
						}),
						state: 'healthy',
						reason: 'healthy'
					}),
					create(CameraHealthSnapshotSchema, {
						id: 'future',
						state: 'future-state',
						reason: 'future-reason'
					})
				]
			}),
			unusedEventMapper
		);

		expect(health.cameras[0]?.pre_recording).toEqual({
			enabled: true,
			active: false,
			selected_stream: 'sub',
			requested_ms: 30_000,
			available_ms: 4_000,
			retained_bytes: 1_048_576,
			reason: 'global pressure'
		});
		expect(health.cameras.map(({ id, state, reason }) => ({ id, state, reason }))).toEqual([
			{ id: 'known', state: 'healthy', reason: 'healthy' },
			{ id: 'future', state: 'unknown', reason: 'unknown' }
		]);
	});

	it('clamps protobuf integers that exceed JavaScript safe precision', () => {
		expect(numeric(BigInt(Number.MAX_SAFE_INTEGER) + 1n)).toBe(Number.MAX_SAFE_INTEGER);
		expect(numeric(42n)).toBe(42);
	});

	it('maps operational health evidence through the canonical event owner', () => {
		const operationalEvent = create(EventSchema, { eventId: 'operational-1' });
		const mappedEvent = { id: 'operational-1' } as RecordingEvent;
		const health = serverHealth(
			create(ServerHealthSnapshotSchema, {
				healthContractVersion: 1,
				totals: create(HealthTotalsSnapshotSchema),
				system: create(SystemHealthSnapshotSchema, {
					process: create(ProcessHealthSnapshotSchema),
					memory: create(MemoryHealthSnapshotSchema),
					load: create(LoadHealthSnapshotSchema)
				}),
				storage: create(StorageHealthSnapshotSchema, {
					demand: create(RecordingDemandHealthSnapshotSchema)
				}),
				webrtc: create(WebRtcHealthSnapshotSchema),
				issues: [
					create(HealthIssueSnapshotSchema, {
						operationalEventId: 'operational-1',
						timelineStart: timestampFromDate(new Date('2026-08-20T01:00:00Z')),
						timelineEnd: timestampFromDate(new Date('2026-08-20T01:02:00Z'))
					})
				],
				operationalEvents: [operationalEvent]
			}),
			(event) => {
				expect(event).toBe(operationalEvent);
				return mappedEvent;
			}
		);

		expect(health.issues[0]).toMatchObject({
			operational_event_id: 'operational-1',
			timeline_start_ms: Date.parse('2026-08-20T01:00:00Z'),
			timeline_end_ms: Date.parse('2026-08-20T01:02:00Z')
		});
		expect(health.operational_events).toEqual([mappedEvent]);
	});
});
