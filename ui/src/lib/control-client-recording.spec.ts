import { create } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import { RecordingControlClient } from './control-client-recording';
import { RecordingControlStateSchema, type Request } from './proto/webrtc_pb';

describe('recording control client', () => {
	it('requires the advertised capability before sending any command', async () => {
		let calls = 0;
		const client = new RecordingControlClient(
			async () => {
				calls++;
				return { case: undefined };
			},
			() => false
		);
		await expect(client.get('camera')).rejects.toThrow('unavailable');
		expect(calls).toBe(0);
	});
	it('carries the current revision and precise TTL without an actor supplied by the client', async () => {
		const requests: Request['command'][] = [];
		const state = create(RecordingControlStateSchema, { sourceId: 'camera', revision: 'epoch:1' });
		const client = new RecordingControlClient(
			async (command) => {
				requests.push(command);
				return { case: 'recordingControlState', value: state };
			},
			() => true
		);
		await client.pause(state, 'inspection', 12_000);
		const command = requests[0];
		if (command?.case !== 'recordingPolicyCommand' || command.value.action.case !== 'setOverride')
			throw new Error('missing override');
		expect(command.value.action.value.expectedRevision).toBe('epoch:1');
		expect(command.value.action.value.ttlMs).toBe(12_000n);
		expect(command.value.action.value.enabled).toBe(false);
		expect('actor' in command.value.action.value).toBe(false);
		await client.clear(state);
		expect(requests[1]?.case).toBe('recordingPolicyCommand');
	});
	it('rejects malformed durations and unexpected source responses', async () => {
		const state = create(RecordingControlStateSchema, { sourceId: 'camera', revision: 'epoch:1' });
		let calls = 0;
		const client = new RecordingControlClient(
			async () => {
				calls++;
				return {
					case: 'recordingControlState',
					value: create(RecordingControlStateSchema, { sourceId: 'other' })
				};
			},
			() => true
		);
		for (const ttl of [0, -1, 0.5, Number.NaN, 86_400_001])
			await expect(client.pause(state, 'reason', ttl)).rejects.toThrow();
		await expect(client.pause(state, 'é'.repeat(129), 1_000)).rejects.toThrow();
		expect(calls).toBe(0);
		await expect(client.get('camera')).rejects.toThrow('Unexpected');
	});
});
