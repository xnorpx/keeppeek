import { create } from '@bufbuild/protobuf';
import {
	ApplyConfigurationPlanSchema,
	ApplyConfigurationTemplateImportSchema,
	CameraBackend as ProtoCameraBackend,
	CameraConfigurationFilterSchema,
	CameraConfigurationPatchSchema,
	CameraDefaultPatchSchema,
	CameraIdListSchema,
	CameraRecordingMode as ProtoCameraRecordingMode,
	CameraTemplateValuesSchema,
	CameraTransport as ProtoCameraTransport,
	ConfigurationActivationStatus,
	ConfigurationChangeSchema,
	ConfigurationCommandSchema,
	ConfigurationImpact as ProtoConfigurationImpact,
	ConfigurationIssueSeverity,
	ConfigurationTargetSelectorSchema,
	ConfigurationTemplateSchema,
	ConfigurationValueSource as ProtoConfigurationValueSource,
	DeleteConfigurationTemplateSchema,
	DuplicateConfigurationTemplateSchema,
	ExportConfigurationTemplatesSchema,
	GetConfigurationSnapshotSchema,
	OptionalBoolUpdateSchema,
	OptionalCameraBackendUpdateSchema,
	OptionalCameraRecordingModeUpdateSchema,
	OptionalCameraTransportUpdateSchema,
	OptionalStringUpdateSchema,
	OptionalUint32UpdateSchema,
	PlanConfigurationChangeSchema,
	PreviewConfigurationTemplateImportSchema,
	SaveConfigurationTemplateSchema,
	type CameraDefaultValues as ProtoCameraDefaultValues,
	type CameraEffectiveConfiguration as ProtoCameraEffectiveConfiguration,
	type CameraSettings as ProtoCameraSettings,
	type ConfigurationApplyResult as ProtoConfigurationApplyResult,
	type ConfigurationIssue as ProtoConfigurationIssue,
	type ConfigurationPlan as ProtoConfigurationPlan,
	type ConfigurationSnapshot as ProtoConfigurationSnapshot,
	type ConfigurationTemplate as ProtoConfigurationTemplate,
	type ConfigurationTemplateImportPreview as ProtoConfigurationTemplateImportPreview,
	type EffectiveBoolValue as ProtoEffectiveBoolValue,
	type EffectiveCameraBackendValue,
	type EffectiveCameraRecordingModeValue,
	type EffectiveCameraTransportValue,
	type EffectiveSecretValue as ProtoEffectiveSecretValue,
	type EffectiveUint32Value,
	type Ok,
	type Request
} from './proto/webrtc_pb';
import type {
	CameraBackend,
	CameraConfigurationPatch,
	CameraDefaultPatch,
	CameraDefaultValues,
	CameraEffectiveConfiguration,
	CameraRecordingMode,
	CameraSettings,
	CameraTransport,
	ConfigurationApplyResult,
	ConfigurationChange,
	ConfigurationImpact,
	ConfigurationIssue,
	ConfigurationPatchValue,
	ConfigurationPlan,
	ConfigurationPlanRequest,
	ConfigurationSnapshot,
	ConfigurationTargetSelector,
	ConfigurationTemplate,
	ConfigurationTemplateImportPreview,
	ConfigurationValueSource,
	EffectiveConfigurationValue,
	EffectiveSecretValue
} from './types';

type SendRequest = (command: Request['command']) => Promise<Ok['result']>;

export class ConfigurationControlClient {
	constructor(private readonly sendRequest: SendRequest) {}

	async getSnapshot(): Promise<ConfigurationSnapshot> {
		return this.completeSnapshot();
	}

	private async getSnapshotPage(pageToken = ''): Promise<ProtoConfigurationSnapshot> {
		const command = create(ConfigurationCommandSchema, {
			action: {
				case: 'get',
				value: create(GetConfigurationSnapshotSchema, { pageSize: 64, pageToken })
			}
		});
		const result = await this.sendRequest({ case: 'configurationCommand', value: command });
		if (result.case !== 'configurationResult' || result.value.result.case !== 'snapshot') {
			throw new Error('Server returned an unexpected configuration snapshot response.');
		}
		return result.value.result.value;
	}

	private async completeSnapshot(
		initialPage?: ProtoConfigurationSnapshot
	): Promise<ConfigurationSnapshot> {
		let rawPage = initialPage ?? (await this.getSnapshotPage());
		let snapshot = configurationSnapshot(rawPage);
		const revision = snapshot.configuration_revision;
		const totalCameraCount = rawPage.totalCameraCount || snapshot.cameras.length;
		const seenTokens = new Set<string>();
		while (rawPage.nextPageToken) {
			if (seenTokens.has(rawPage.nextPageToken)) {
				throw new Error('Server repeated a configuration snapshot page token.');
			}
			seenTokens.add(rawPage.nextPageToken);
			rawPage = await this.getSnapshotPage(rawPage.nextPageToken);
			const next = configurationSnapshot(rawPage);
			if (next.configuration_revision !== revision) {
				throw new Error('Configuration changed while snapshot pages were loading.');
			}
			snapshot = { ...snapshot, cameras: [...snapshot.cameras, ...next.cameras] };
		}
		if (snapshot.cameras.length !== totalCameraCount) {
			throw new Error(
				`Server returned ${snapshot.cameras.length} of ${totalCameraCount} configuration cameras.`
			);
		}
		return snapshot;
	}

	async saveTemplate(
		expectedConfigurationRevision: string,
		template: ConfigurationTemplate,
		expectedTemplateVersion?: number
	): Promise<ConfigurationTemplate> {
		const command = create(ConfigurationCommandSchema, {
			action: {
				case: 'saveTemplate',
				value: create(SaveConfigurationTemplateSchema, {
					expectedConfigurationRevision,
					template: protoTemplate(template),
					expectedTemplateVersion:
						expectedTemplateVersion === undefined ? undefined : BigInt(expectedTemplateVersion)
				})
			}
		});
		const result = await this.sendRequest({ case: 'configurationCommand', value: command });
		if (result.case !== 'configurationResult' || result.value.result.case !== 'template') {
			throw new Error('Server returned an unexpected configuration template response.');
		}
		return configurationTemplate(result.value.result.value);
	}

	async duplicateTemplate(
		expectedConfigurationRevision: string,
		templateId: string,
		name: string
	): Promise<ConfigurationTemplate> {
		const command = create(ConfigurationCommandSchema, {
			action: {
				case: 'duplicateTemplate',
				value: create(DuplicateConfigurationTemplateSchema, {
					expectedConfigurationRevision,
					templateId,
					name
				})
			}
		});
		const result = await this.sendRequest({ case: 'configurationCommand', value: command });
		if (result.case !== 'configurationResult' || result.value.result.case !== 'template') {
			throw new Error('Server returned an unexpected duplicated template response.');
		}
		return configurationTemplate(result.value.result.value);
	}

	async deleteTemplate(
		expectedConfigurationRevision: string,
		templateId: string
	): Promise<ConfigurationSnapshot> {
		const command = create(ConfigurationCommandSchema, {
			action: {
				case: 'deleteTemplate',
				value: create(DeleteConfigurationTemplateSchema, {
					expectedConfigurationRevision,
					templateId
				})
			}
		});
		const result = await this.sendRequest({ case: 'configurationCommand', value: command });
		if (result.case !== 'configurationResult' || result.value.result.case !== 'snapshot') {
			throw new Error('Server returned an unexpected post-delete configuration snapshot.');
		}
		return this.completeSnapshot(result.value.result.value);
	}

	async plan(request: ConfigurationPlanRequest): Promise<ConfigurationPlan> {
		const command = create(ConfigurationCommandSchema, {
			action: {
				case: 'plan',
				value: create(PlanConfigurationChangeSchema, {
					expectedConfigurationRevision: request.expected_configuration_revision,
					targets: request.targets ? protoTargets(request.targets) : undefined,
					change: protoChange(request.change)
				})
			}
		});
		const result = await this.sendRequest({ case: 'configurationCommand', value: command });
		if (result.case !== 'configurationResult' || result.value.result.case !== 'plan') {
			throw new Error('Server returned an unexpected configuration plan response.');
		}
		return configurationPlan(result.value.result.value);
	}

	async apply(
		planId: string,
		expectedConfigurationRevision: string
	): Promise<ConfigurationApplyResult> {
		const command = create(ConfigurationCommandSchema, {
			action: {
				case: 'apply',
				value: create(ApplyConfigurationPlanSchema, {
					planId,
					expectedConfigurationRevision
				})
			}
		});
		const result = await this.sendRequest({ case: 'configurationCommand', value: command });
		if (result.case !== 'configurationResult' || result.value.result.case !== 'applied') {
			throw new Error('Server returned an unexpected configuration apply response.');
		}
		const snapshot = result.value.result.value.snapshot
			? await this.completeSnapshot(result.value.result.value.snapshot)
			: await this.getSnapshot();
		return configurationApplyResult(result.value.result.value, snapshot);
	}

	async exportTemplates(templateIds: string[] = []): Promise<string> {
		const command = create(ConfigurationCommandSchema, {
			action: {
				case: 'exportTemplates',
				value: create(ExportConfigurationTemplatesSchema, { templateIds })
			}
		});
		const result = await this.sendRequest({ case: 'configurationCommand', value: command });
		if (result.case !== 'configurationResult' || result.value.result.case !== 'exportedTemplates') {
			throw new Error('Server returned an unexpected template export response.');
		}
		return result.value.result.value.documentJson;
	}

	async previewImport(
		expectedConfigurationRevision: string,
		documentJson: string
	): Promise<ConfigurationTemplateImportPreview> {
		const command = create(ConfigurationCommandSchema, {
			action: {
				case: 'previewImport',
				value: create(PreviewConfigurationTemplateImportSchema, {
					expectedConfigurationRevision,
					documentJson
				})
			}
		});
		const result = await this.sendRequest({ case: 'configurationCommand', value: command });
		if (result.case !== 'configurationResult' || result.value.result.case !== 'importPreview') {
			throw new Error('Server returned an unexpected template import preview.');
		}
		return configurationImportPreview(result.value.result.value);
	}

	async applyImport(
		previewId: string,
		expectedConfigurationRevision: string
	): Promise<ConfigurationSnapshot> {
		const command = create(ConfigurationCommandSchema, {
			action: {
				case: 'applyImport',
				value: create(ApplyConfigurationTemplateImportSchema, {
					previewId,
					expectedConfigurationRevision
				})
			}
		});
		const result = await this.sendRequest({ case: 'configurationCommand', value: command });
		if (result.case !== 'configurationResult' || result.value.result.case !== 'snapshot') {
			throw new Error('Server returned an unexpected post-import configuration snapshot.');
		}
		return this.completeSnapshot(result.value.result.value);
	}
}

function protoTargets(targets: ConfigurationTargetSelector) {
	const selection = (() => {
		if (targets.mode === 'camera-ids') {
			return {
				case: 'cameraIds' as const,
				value: create(CameraIdListSchema, { cameraIds: targets.camera_ids })
			};
		}
		if (targets.mode === 'filtered-cameras') {
			return {
				case: 'filteredCameras' as const,
				value: create(CameraConfigurationFilterSchema, {
					search: targets.search,
					backend: targets.backend ? protoBackend(targets.backend) : undefined,
					recordingMode: targets.recording_mode
						? protoRecordingMode(targets.recording_mode)
						: undefined
				})
			};
		}
		if (targets.mode === 'group') {
			return { case: 'groupId' as const, value: targets.group_id };
		}
		return { case: 'allCameras' as const, value: true };
	})();
	return create(ConfigurationTargetSelectorSchema, { selection });
}

function protoChange(change: ConfigurationChange) {
	if (change.mode === 'template') {
		return create(ConfigurationChangeSchema, {
			change: { case: 'templateId', value: change.template_id }
		});
	}
	if (change.mode === 'defaults') {
		return create(ConfigurationChangeSchema, {
			change: { case: 'defaults', value: protoDefaultPatch(change.patch) }
		});
	}
	return create(ConfigurationChangeSchema, {
		change: { case: 'patch', value: protoCameraPatch(change.patch) }
	});
}

function protoCameraPatch(patch: CameraConfigurationPatch) {
	return create(CameraConfigurationPatchSchema, {
		displayName: stringPatch(patch.display_name),
		manufacturer: stringPatch(patch.manufacturer),
		usernameSecretReference: stringPatch(patch.username_secret_reference),
		passwordSecretReference: stringPatch(patch.password_secret_reference),
		onvifPort: numberPatch(patch.onvif_port),
		httpPort: numberPatch(patch.http_port),
		mainRtspUrl: stringPatch(patch.main_rtsp_url),
		subRtspUrl: stringPatch(patch.sub_rtsp_url),
		uidSecretReference: stringPatch(patch.uid_secret_reference),
		backend: backendPatch(patch.backend),
		transport: transportPatch(patch.transport),
		recordGenericMotionEvents: booleanPatch(patch.record_generic_motion_events),
		recordingMode: recordingModePatch(patch.recording_mode),
		eventRecordingDurationSecs: numberPatch(patch.event_recording_duration_secs)
	});
}

function protoDefaultPatch(patch: CameraDefaultPatch) {
	return create(CameraDefaultPatchSchema, {
		usernameSecretReference: stringPatch(patch.username_secret_reference),
		passwordSecretReference: stringPatch(patch.password_secret_reference),
		backend: backendPatch(patch.backend),
		transport: transportPatch(patch.transport),
		recordGenericMotionEvents: booleanPatch(patch.record_generic_motion_events),
		recordingMode: recordingModePatch(patch.recording_mode),
		eventRecordingDurationSecs: numberPatch(patch.event_recording_duration_secs)
	});
}

function stringPatch(value: ConfigurationPatchValue<string> | undefined) {
	if (!value) return undefined;
	return create(OptionalStringUpdateSchema, {
		value:
			value.operation === 'clear'
				? { case: 'clear', value: true }
				: { case: 'set', value: value.value }
	});
}

function numberPatch(value: ConfigurationPatchValue<number> | undefined) {
	if (!value) return undefined;
	return create(OptionalUint32UpdateSchema, {
		value:
			value.operation === 'clear'
				? { case: 'clear', value: true }
				: { case: 'set', value: value.value }
	});
}

function booleanPatch(value: ConfigurationPatchValue<boolean> | undefined) {
	if (!value) return undefined;
	return create(OptionalBoolUpdateSchema, {
		value:
			value.operation === 'clear'
				? { case: 'clear', value: true }
				: { case: 'set', value: value.value }
	});
}

function backendPatch(value: ConfigurationPatchValue<CameraBackend> | undefined) {
	if (!value) return undefined;
	return create(OptionalCameraBackendUpdateSchema, {
		value:
			value.operation === 'clear'
				? { case: 'clear', value: true }
				: { case: 'set', value: protoBackend(value.value) }
	});
}

function transportPatch(value: ConfigurationPatchValue<CameraTransport> | undefined) {
	if (!value) return undefined;
	return create(OptionalCameraTransportUpdateSchema, {
		value:
			value.operation === 'clear'
				? { case: 'clear', value: true }
				: { case: 'set', value: protoTransport(value.value) }
	});
}

function recordingModePatch(value: ConfigurationPatchValue<CameraRecordingMode> | undefined) {
	if (!value) return undefined;
	return create(OptionalCameraRecordingModeUpdateSchema, {
		value:
			value.operation === 'clear'
				? { case: 'clear', value: true }
				: { case: 'set', value: protoRecordingMode(value.value) }
	});
}

function protoTemplate(template: ConfigurationTemplate) {
	return create(ConfigurationTemplateSchema, {
		templateId: template.template_id,
		version: BigInt(template.version),
		name: template.name,
		description: template.description,
		values: create(CameraTemplateValuesSchema, {
			usernameSecretReference: template.values.username_secret_reference,
			passwordSecretReference: template.values.password_secret_reference,
			onvifPort: template.values.onvif_port,
			httpPort: template.values.http_port,
			backend: template.values.backend ? protoBackend(template.values.backend) : undefined,
			transport: template.values.transport ? protoTransport(template.values.transport) : undefined,
			recordGenericMotionEvents: template.values.record_generic_motion_events,
			recordingMode: template.values.recording_mode
				? protoRecordingMode(template.values.recording_mode)
				: undefined,
			eventRecordingDurationSecs: template.values.event_recording_duration_secs
		}),
		createdAtMs: BigInt(template.created_at_ms),
		updatedAtMs: BigInt(template.updated_at_ms)
	});
}

function configurationSnapshot(snapshot: ProtoConfigurationSnapshot): ConfigurationSnapshot {
	if (snapshot.contractVersion !== 1) {
		throw new Error(
			`Server returned unsupported configuration contract version ${snapshot.contractVersion}.`
		);
	}
	if (!snapshot.defaults || !snapshot.limits) {
		throw new Error('Server returned incomplete configuration evidence.');
	}
	return {
		contract_version: snapshot.contractVersion,
		configuration_revision: snapshot.configurationRevision,
		defaults: cameraDefaults(snapshot.defaults),
		cameras: snapshot.cameras.map(cameraEffectiveConfiguration),
		templates: snapshot.templates.map(configurationTemplate),
		limits: {
			maximum_templates: snapshot.limits.maximumTemplates,
			maximum_template_name_bytes: snapshot.limits.maximumTemplateNameBytes,
			maximum_template_description_bytes: snapshot.limits.maximumTemplateDescriptionBytes,
			maximum_plan_targets: snapshot.limits.maximumPlanTargets,
			maximum_import_bytes: snapshot.limits.maximumImportBytes
		},
		domains: snapshot.domains.map((domain) => ({
			domain_id: domain.domainId,
			label: domain.label,
			owner_path: domain.ownerPath,
			capability_id: domain.capabilityId,
			readable: domain.readable,
			mutable: domain.mutable,
			unavailable_reason: domain.unavailableReason ?? null
		}))
	};
}

function cameraDefaults(defaults: ProtoCameraDefaultValues): CameraDefaultValues {
	return {
		username_configured: defaults.usernameConfigured,
		password_configured: defaults.passwordConfigured,
		configured_backend:
			defaults.configuredBackend === undefined ? null : cameraBackend(defaults.configuredBackend),
		effective_backend: cameraBackend(defaults.effectiveBackend),
		configured_transport:
			defaults.configuredTransport === undefined
				? null
				: cameraTransport(defaults.configuredTransport),
		effective_transport: cameraTransport(defaults.effectiveTransport),
		configured_record_generic_motion_events: defaults.configuredRecordGenericMotionEvents ?? null,
		effective_record_generic_motion_events: defaults.effectiveRecordGenericMotionEvents,
		configured_recording_mode:
			defaults.configuredRecordingMode === undefined
				? null
				: cameraRecordingMode(defaults.configuredRecordingMode),
		effective_recording_mode: cameraRecordingMode(defaults.effectiveRecordingMode),
		configured_event_recording_duration_secs: defaults.configuredEventRecordingDurationSecs ?? null,
		effective_event_recording_duration_secs: defaults.effectiveEventRecordingDurationSecs
	};
}

function cameraEffectiveConfiguration(
	camera: ProtoCameraEffectiveConfiguration
): CameraEffectiveConfiguration {
	if (
		!camera.camera ||
		!camera.username ||
		!camera.password ||
		!camera.backend ||
		!camera.transport ||
		!camera.recordGenericMotionEvents ||
		!camera.recordingMode ||
		!camera.eventRecordingDurationSecs
	) {
		throw new Error('Server returned incomplete camera configuration evidence.');
	}
	return {
		camera: cameraSettings(camera.camera),
		group_ids: [...camera.groupIds],
		username: effectiveSecret(camera.username),
		password: effectiveSecret(camera.password),
		backend: effectiveValue(camera.backend, cameraBackend),
		transport: effectiveValue(camera.transport, cameraTransport),
		record_generic_motion_events: effectiveValue(
			camera.recordGenericMotionEvents,
			(value: boolean) => value
		),
		recording_mode: effectiveValue(camera.recordingMode, cameraRecordingMode),
		event_recording_duration_secs: effectiveValue(
			camera.eventRecordingDurationSecs,
			(value: number) => value
		)
	};
}

function effectiveSecret(value: ProtoEffectiveSecretValue): EffectiveSecretValue {
	return {
		default_configured: value.defaultConfigured,
		override_configured: value.overrideConfigured,
		effective_configured: value.effectiveConfigured,
		source: configurationValueSource(value.source),
		runtime_applied: value.runtimeApplied,
		warning: value.warning ?? null
	};
}

function effectiveValue<TProto, TValue>(
	value:
		| EffectiveCameraBackendValue
		| EffectiveCameraTransportValue
		| EffectiveCameraRecordingModeValue
		| ProtoEffectiveBoolValue
		| EffectiveUint32Value,
	map: (value: TProto) => TValue
): EffectiveConfigurationValue<TValue> {
	const typed = value as typeof value & {
		configuredDefault?: TProto;
		cameraOverride?: TProto;
		effective: TProto;
	};
	return {
		configured_default: typed.configuredDefault === undefined ? null : map(typed.configuredDefault),
		camera_override: typed.cameraOverride === undefined ? null : map(typed.cameraOverride),
		effective: map(typed.effective),
		source: configurationValueSource(value.source),
		runtime_applied: value.runtimeApplied,
		warning: value.warning ?? null
	};
}

function configurationTemplate(template: ProtoConfigurationTemplate): ConfigurationTemplate {
	if (!template.values) throw new Error('Server returned a template without values.');
	return {
		template_id: template.templateId,
		version: Number(template.version),
		name: template.name,
		description: template.description,
		values: {
			username_secret_reference: template.values.usernameSecretReference,
			password_secret_reference: template.values.passwordSecretReference,
			onvif_port: template.values.onvifPort,
			http_port: template.values.httpPort,
			backend:
				template.values.backend === undefined ? undefined : cameraBackend(template.values.backend),
			transport:
				template.values.transport === undefined
					? undefined
					: cameraTransport(template.values.transport),
			record_generic_motion_events: template.values.recordGenericMotionEvents,
			recording_mode:
				template.values.recordingMode === undefined
					? undefined
					: cameraRecordingMode(template.values.recordingMode),
			event_recording_duration_secs: template.values.eventRecordingDurationSecs
		},
		created_at_ms: Number(template.createdAtMs),
		updated_at_ms: Number(template.updatedAtMs)
	};
}

function configurationPlan(plan: ProtoConfigurationPlan): ConfigurationPlan {
	return {
		plan_id: plan.planId,
		configuration_revision: plan.configurationRevision,
		expires_at_ms: Number(plan.expiresAtMs),
		authoritative_target_count: plan.authoritativeTargetCount,
		targets: plan.targets.map((target) => ({
			camera_id: target.cameraId,
			display_name: target.displayName,
			group_ids: [...target.groupIds],
			skipped: target.skipped,
			skip_reason: target.skipReason ?? null
		})),
		changes: plan.changes.map((change) => ({
			camera_id: change.cameraId ?? null,
			field: change.field,
			old_configured_value: change.oldConfiguredValue,
			old_effective_value: change.oldEffectiveValue,
			new_configured_value: change.newConfiguredValue,
			new_effective_value: change.newEffectiveValue,
			source: configurationValueSource(change.source),
			secret: change.secret
		})),
		issues: plan.issues.map(configurationIssue),
		impact: configurationImpact(plan.impact),
		valid: plan.valid,
		apply_semantics: plan.applySemantics
	};
}

function configurationApplyResult(
	result: ProtoConfigurationApplyResult,
	snapshot: ConfigurationSnapshot
): ConfigurationApplyResult {
	return {
		plan_id: result.planId,
		configuration_committed: result.configurationCommitted,
		snapshot,
		activations: result.activations.map((activation) => ({
			camera_id: activation.cameraId,
			status:
				activation.status === ConfigurationActivationStatus.APPLIED
					? 'applied'
					: activation.status === ConfigurationActivationStatus.RECONNECT_REQUIRED
						? 'reconnect-required'
						: activation.status === ConfigurationActivationStatus.RESTART_REQUIRED
							? 'restart-required'
							: 'failed',
			detail: activation.detail ?? null
		})),
		impact: configurationImpact(result.impact)
	};
}

function configurationImportPreview(
	preview: ProtoConfigurationTemplateImportPreview
): ConfigurationTemplateImportPreview {
	return {
		preview_id: preview.previewId,
		configuration_revision: preview.configurationRevision,
		expires_at_ms: Number(preview.expiresAtMs),
		templates: preview.templates.map(configurationTemplate),
		issues: preview.issues.map(configurationIssue),
		valid: preview.valid
	};
}

function configurationIssue(issue: ProtoConfigurationIssue): ConfigurationIssue {
	return {
		camera_id: issue.cameraId ?? null,
		field: issue.field,
		severity:
			issue.severity === ConfigurationIssueSeverity.INFO
				? 'info'
				: issue.severity === ConfigurationIssueSeverity.WARNING
					? 'warning'
					: 'error',
		code: issue.code,
		message: issue.message,
		required_capability: issue.requiredCapability ?? null
	};
}

function configurationValueSource(source: ProtoConfigurationValueSource): ConfigurationValueSource {
	if (source === ProtoConfigurationValueSource.DEFAULT) return 'default';
	if (source === ProtoConfigurationValueSource.TEMPLATE) return 'template';
	if (source === ProtoConfigurationValueSource.OVERRIDE) return 'override';
	return 'built-in';
}

function configurationImpact(impact: ProtoConfigurationImpact): ConfigurationImpact {
	if (impact === ProtoConfigurationImpact.RECONNECT_CAMERA) return 'reconnect-camera';
	if (impact === ProtoConfigurationImpact.RESTART_COMPONENT) return 'restart-component';
	if (impact === ProtoConfigurationImpact.RESTART_SERVER) return 'restart-server';
	return 'immediate';
}

function protoBackend(backend: CameraBackend): ProtoCameraBackend {
	if (backend === 'retina') return ProtoCameraBackend.RETINA;
	if (backend === 'reo-proto') return ProtoCameraBackend.REO_PROTO;
	return ProtoCameraBackend.AUTO;
}

function cameraBackend(backend: ProtoCameraBackend): CameraBackend {
	if (backend === ProtoCameraBackend.RETINA) return 'retina';
	if (backend === ProtoCameraBackend.REO_PROTO) return 'reo-proto';
	return 'auto';
}

function protoTransport(transport: CameraTransport): ProtoCameraTransport {
	return transport === 'udp' ? ProtoCameraTransport.UDP : ProtoCameraTransport.TCP;
}

function cameraTransport(transport: ProtoCameraTransport): CameraTransport {
	return transport === ProtoCameraTransport.UDP ? 'udp' : 'tcp';
}

function protoRecordingMode(mode: CameraRecordingMode): ProtoCameraRecordingMode {
	if (mode === 'off') return ProtoCameraRecordingMode.OFF;
	if (mode === 'sub') return ProtoCameraRecordingMode.SUB;
	if (mode === 'main') return ProtoCameraRecordingMode.MAIN;
	if (mode === 'both') return ProtoCameraRecordingMode.BOTH;
	return ProtoCameraRecordingMode.EVENT_BOOST;
}

function cameraRecordingMode(mode: ProtoCameraRecordingMode): CameraRecordingMode {
	if (mode === ProtoCameraRecordingMode.OFF) return 'off';
	if (mode === ProtoCameraRecordingMode.SUB) return 'sub';
	if (mode === ProtoCameraRecordingMode.MAIN) return 'main';
	if (mode === ProtoCameraRecordingMode.BOTH) return 'both';
	return 'event-boost';
}

function cameraSettings(camera: ProtoCameraSettings): CameraSettings {
	return {
		id: camera.id,
		ip: camera.ip,
		display_name: camera.displayName ?? null,
		manufacturer_override: camera.manufacturerOverride ?? null,
		username_configured: camera.usernameConfigured,
		password_configured: camera.passwordConfigured,
		onvif_port: camera.onvifPort ?? null,
		http_port: camera.httpPort ?? null,
		main_rtsp_url: camera.mainRtspUrl ?? null,
		sub_rtsp_url: camera.subRtspUrl ?? null,
		uid_configured: camera.uidConfigured,
		backend: cameraBackend(camera.backend),
		transport: cameraTransport(camera.transport),
		record_generic_motion_events: camera.recordGenericMotionEvents,
		recording_mode: cameraRecordingMode(camera.recordingMode),
		event_recording_duration_secs: camera.eventRecordingDurationSecs || 60,
		health: (camera.health ?? null) as CameraSettings['health'],
		model: camera.model ?? null
	};
}
