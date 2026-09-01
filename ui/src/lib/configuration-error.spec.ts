import { create, toBinary } from '@bufbuild/protobuf';
import { AnySchema } from '@bufbuild/protobuf/wkt';
import { describe, expect, it } from 'vitest';
import {
	ConfigurationErrorCode,
	ConfigurationErrorSchema,
	ConfigurationIssueSchema,
	ConfigurationIssueSeverity,
	ErrorCode,
	ErrorSchema
} from './proto/webrtc_pb';
import { ConfigurationRequestError, decodeConfigurationRequestError } from './configuration-error';

describe('configuration request errors', () => {
	it('decodes the current revision and field-addressable issues', () => {
		const detail = create(ConfigurationErrorSchema, {
			code: ConfigurationErrorCode.CONFLICT,
			currentConfigurationRevision: 'revision-9',
			issues: [
				create(ConfigurationIssueSchema, {
					cameraId: '192.0.2.10',
					field: 'backend',
					severity: ConfigurationIssueSeverity.ERROR,
					code: 'backend_conflict',
					message: 'Backend changed on the server.'
				})
			]
		});
		const error = create(ErrorSchema, {
			code: ErrorCode.REJECTED,
			message: 'configuration changed after this editor was opened',
			details: [
				create(AnySchema, {
					typeUrl: 'type.keeppeek.dev/configuration-error.v1',
					value: toBinary(ConfigurationErrorSchema, detail)
				})
			]
		});

		const decoded = decodeConfigurationRequestError(error);

		expect(decoded).toBeInstanceOf(ConfigurationRequestError);
		expect(decoded).toMatchObject({
			code: ConfigurationErrorCode.CONFLICT,
			currentRevision: 'revision-9',
			issues: [
				{
					cameraId: '192.0.2.10',
					field: 'backend',
					severity: 'error',
					code: 'backend_conflict',
					message: 'Backend changed on the server.'
				}
			]
		});
	});

	it('ignores malformed typed details', () => {
		const error = create(ErrorSchema, {
			code: ErrorCode.REJECTED,
			message: 'conflict',
			details: [
				create(AnySchema, {
					typeUrl: 'type.keeppeek.dev/configuration-error.v1',
					value: Uint8Array.from([255])
				})
			]
		});

		expect(decodeConfigurationRequestError(error)).toBeNull();
	});
});
