import { create } from '@bufbuild/protobuf';
import {
	RecordingPolicyCommandSchema,
	RecordingOverrideSource,
	type RecordingControlState,
	type RecordingPolicyCommand,
	type Request,
	type Ok
} from './proto/webrtc_pb';

export const RECORDING_CONTROL_CAPABILITY = 'keeppeek.recording-control.v1';

export class RecordingControlClient {
	constructor(
		private readonly send: (command: Request['command']) => Promise<Ok['result']>,
		private readonly supported: () => boolean
	) {}

	get(sourceId: string): Promise<RecordingControlState> {
		return this.request(
			sourceId,
			create(RecordingPolicyCommandSchema, { action: { case: 'get', value: {} } }).action
		);
	}

	async pause(
		state: RecordingControlState,
		reason: string,
		ttlMs: number
	): Promise<RecordingControlState> {
		if (
			!reason.trim() ||
			new TextEncoder().encode(reason).length > 256 ||
			!Number.isSafeInteger(ttlMs) ||
			ttlMs < 1 ||
			ttlMs > 86_400_000
		)
			throw new Error('Enter a reason and a duration of up to 24 hours.');
		return this.request(
			state.sourceId,
			create(RecordingPolicyCommandSchema, {
				action: {
					case: 'setOverride',
					value: {
						expectedRevision: state.revision,
						enabled: false,
						source: RecordingOverrideSource.MANUAL,
						reason,
						ttlMs: BigInt(ttlMs)
					}
				}
			}).action
		);
	}

	clear(state: RecordingControlState): Promise<RecordingControlState> {
		return this.request(
			state.sourceId,
			create(RecordingPolicyCommandSchema, {
				action: { case: 'clearOverride', value: { expectedRevision: state.revision } }
			}).action
		);
	}

	private async request(
		sourceId: string,
		action: RecordingPolicyCommand['action']
	): Promise<RecordingControlState> {
		if (!this.supported())
			throw new Error('Temporary recording controls are unavailable on this server.');
		const result = await this.send({
			case: 'recordingPolicyCommand',
			value: create(RecordingPolicyCommandSchema, { sourceId, action })
		});
		if (
			result.case !== 'recordingControlState' ||
			result.value.sourceId !== sourceId ||
			!result.value.revision
		)
			throw new Error('Unexpected recording control response.');
		return result.value;
	}
}
