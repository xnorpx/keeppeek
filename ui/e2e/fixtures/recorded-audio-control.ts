import { fromBinary } from '@bufbuild/protobuf';
import type { Page } from '@playwright/test';
import { ControlEnvelopeSchema } from '../../src/lib/proto/webrtc_pb';

type ControlError = { code: number; message: string; requestId: string };
type ControlEvidence = { errors: ControlError[]; overflow: boolean };
type ProbeWindow = Window & {
	recordedAudioControlError: (payload: number[] | null) => Promise<void>;
};

const maximumBytes = 4096;
const maximumErrors = 32;
const safeMessages = new Set([
	'stored media cursor changed during playback update',
	'stored media codec changed during refill',
	'stored media cursor, source, stream, or timestamp was not found',
	'Recorded audio correction requires compatible continuous AAC media within the supported limits.'
]);

function installProbe({
	maximumBytes,
	maximumPending
}: {
	maximumBytes: number;
	maximumPending: number;
}) {
	const createChannel = RTCPeerConnection.prototype.createDataChannel;
	let pending = 0;
	let overflowReported = false;
	const forward = (window as unknown as ProbeWindow).recordedAudioControlError;
	const reportOverflow = () => {
		if (overflowReported) return;
		overflowReported = true;
		void forward(null).catch(() => undefined);
	};
	RTCPeerConnection.prototype.createDataChannel = function (label, options) {
		const channel = createChannel.call(this, label, options);
		if (label !== 'control-channel') return channel;
		channel.addEventListener('message', (event: MessageEvent<unknown>) => {
			const data = event.data;
			const bytes =
				data instanceof ArrayBuffer
					? new Uint8Array(data)
					: ArrayBuffer.isView(data)
						? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
						: null;
			if (!bytes || bytes.byteLength > maximumBytes || pending >= maximumPending) {
				reportOverflow();
				return;
			}
			pending += 1;
			void forward([...bytes])
				.catch(reportOverflow)
				.finally(() => (pending -= 1));
		});
		return channel;
	};
}

export async function installRecordedAudioControlProbe(page: Page): Promise<() => ControlEvidence> {
	const evidence: ControlEvidence = { errors: [], overflow: false };
	await page.exposeBinding('recordedAudioControlError', ({ frame }, payload: unknown) => {
		if (frame !== page.mainFrame()) return;
		if (
			!Array.isArray(payload) ||
			payload.length > maximumBytes ||
			!payload.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255)
		) {
			evidence.overflow = true;
			return;
		}
		try {
			const { message } = fromBinary(ControlEnvelopeSchema, Uint8Array.from(payload), {
				readUnknownFields: false
			});
			if (message.case !== 'response' || message.value.result.case !== 'error') return;
			if (evidence.errors.length === maximumErrors) {
				evidence.overflow = true;
				return;
			}
			const error = message.value.result.value;
			evidence.errors.push({
				code: error.code,
				// ponytail: retain reviewed static messages; unknown text needs review before artifact capture.
				message: safeMessages.has(error.message)
					? error.message
					: '[unrecognized error text omitted]',
				requestId: message.value.requestId.toString()
			});
		} catch {
			evidence.overflow = true;
		}
	});
	await page.addInitScript(installProbe, { maximumBytes, maximumPending: 8 });
	return () => ({
		errors: evidence.errors.map((error) => ({ ...error })),
		overflow: evidence.overflow
	});
}
