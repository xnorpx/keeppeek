import { fromBinary } from '@bufbuild/protobuf';
import {
	StateStoreErrorCode,
	StateStoreErrorSchema,
	type Error as ControlError
} from './proto/webrtc_pb';

const stateStoreErrorTypeUrl = 'type.googleapis.com/keeppeek.webrtc.v1.StateStoreError';

export class StateStoreRequestError extends Error {
	readonly namespace: string;
	readonly key: string;
	readonly code: StateStoreErrorCode;
	readonly currentRevision: bigint | undefined;

	constructor(
		message: string,
		detail: {
			namespace: string;
			key: string;
			code: StateStoreErrorCode;
			currentRevision?: bigint;
		}
	) {
		const revisionSuffix =
			detail.currentRevision === undefined
				? ''
				: ` (current revision ${detail.currentRevision.toString()})`;
		super(`${message}${revisionSuffix}`);
		this.name = 'StateStoreRequestError';
		this.namespace = detail.namespace;
		this.key = detail.key;
		this.code = detail.code;
		this.currentRevision = detail.currentRevision;
	}
}

export function decodeStateStoreRequestError(error: ControlError): StateStoreRequestError | null {
	for (const detail of error.details) {
		if (detail.typeUrl !== stateStoreErrorTypeUrl) continue;
		try {
			const decoded = fromBinary(StateStoreErrorSchema, detail.value);
			if (
				decoded.namespace.length === 0 ||
				decoded.key.length === 0 ||
				decoded.code === StateStoreErrorCode.UNSPECIFIED
			) {
				continue;
			}
			return new StateStoreRequestError(error.message, decoded);
		} catch {
			continue;
		}
	}
	return null;
}
