import { create, toBinary } from '@bufbuild/protobuf';
import { AnySchema } from '@bufbuild/protobuf/wkt';
import { describe, expect, it } from 'vitest';
import { ErrorSchema, StateStoreErrorCode, StateStoreErrorSchema } from './proto/webrtc_pb';
import { StateStoreRequestError, decodeStateStoreRequestError } from './state-store-error';

describe('StateStore request errors', () => {
	it('decodes a typed conflict with the current server revision', () => {
		const detail = create(StateStoreErrorSchema, {
			namespace: 'keeppeek.peek-layouts',
			key: 'registry',
			code: StateStoreErrorCode.CONFLICT,
			currentRevision: 12n
		});
		const response = create(ErrorSchema, {
			message: 'Peek layout registry revision conflict',
			details: [
				create(AnySchema, {
					typeUrl: 'type.googleapis.com/keeppeek.webrtc.v1.StateStoreError',
					value: toBinary(StateStoreErrorSchema, detail)
				})
			]
		});

		const error = decodeStateStoreRequestError(response);

		expect(error).toBeInstanceOf(StateStoreRequestError);
		expect(error).toMatchObject({
			namespace: 'keeppeek.peek-layouts',
			key: 'registry',
			code: StateStoreErrorCode.CONFLICT,
			currentRevision: 12n,
			message: 'Peek layout registry revision conflict (current revision 12)'
		});
	});
});
