import { fromBinary } from '@bufbuild/protobuf';
import {
	ConfigurationErrorCode,
	ConfigurationErrorSchema,
	ConfigurationIssueSeverity,
	type Error as ControlError
} from './proto/webrtc_pb';

const configurationErrorTypeUrl = 'type.keeppeek.dev/configuration-error.v1';

export type ConfigurationFieldIssue = {
	cameraId: string | null;
	field: string;
	severity: 'info' | 'warning' | 'error';
	code: string;
	message: string;
	requiredCapability: string | null;
};

export class ConfigurationRequestError extends Error {
	readonly code: ConfigurationErrorCode;
	readonly currentRevision: string;
	readonly issues: ConfigurationFieldIssue[];

	constructor(
		message: string,
		detail: {
			code: ConfigurationErrorCode;
			currentRevision: string;
			issues: ConfigurationFieldIssue[];
		}
	) {
		const revision = detail.currentRevision ? ` (current revision ${detail.currentRevision})` : '';
		super(`${message}${revision}`);
		this.name = 'ConfigurationRequestError';
		this.code = detail.code;
		this.currentRevision = detail.currentRevision;
		this.issues = detail.issues;
	}
}

export function decodeConfigurationRequestError(
	error: ControlError
): ConfigurationRequestError | null {
	for (const detail of error.details) {
		if (detail.typeUrl !== configurationErrorTypeUrl) continue;
		try {
			const decoded = fromBinary(ConfigurationErrorSchema, detail.value);
			if (decoded.code === ConfigurationErrorCode.UNSPECIFIED) continue;
			return new ConfigurationRequestError(error.message, {
				code: decoded.code,
				currentRevision: decoded.currentConfigurationRevision,
				issues: decoded.issues.map((issue) => ({
					cameraId: issue.cameraId ?? null,
					field: issue.field,
					severity: configurationIssueSeverity(issue.severity),
					code: issue.code,
					message: issue.message,
					requiredCapability: issue.requiredCapability ?? null
				}))
			});
		} catch {
			continue;
		}
	}
	return null;
}

function configurationIssueSeverity(
	severity: ConfigurationIssueSeverity
): ConfigurationFieldIssue['severity'] {
	if (severity === ConfigurationIssueSeverity.INFO) return 'info';
	if (severity === ConfigurationIssueSeverity.WARNING) return 'warning';
	return 'error';
}
