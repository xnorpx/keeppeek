import { create } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import {
	CameraBackend as ProtoCameraBackend,
	CameraDefaultValuesSchema,
	ConfigurationApplyResultSchema,
	ConfigurationLimitsSchema,
	ConfigurationPlanSchema,
	ConfigurationResultSchema,
	ConfigurationSnapshotSchema,
	type Ok,
	type Request
} from './proto/webrtc_pb';
import { ConfigurationControlClient } from './control-client-configuration';

describe('configuration control client', () => {
	it('maps snapshots and encodes inherited camera values as clear operations', async () => {
		const commands: Request['command'][] = [];
		const send = async (command: Request['command']): Promise<Ok['result']> => {
			commands.push(command);
			if (command.case === 'configurationCommand' && command.value.action.case === 'get') {
				return {
					case: 'configurationResult',
					value: create(ConfigurationResultSchema, {
						result: {
							case: 'snapshot',
							value: create(ConfigurationSnapshotSchema, {
								contractVersion: 1,
								configurationRevision: 'revision-4',
								defaults: create(CameraDefaultValuesSchema, {
									configuredBackend: ProtoCameraBackend.REO_PROTO,
									effectiveBackend: ProtoCameraBackend.REO_PROTO
								}),
								limits: create(ConfigurationLimitsSchema, {
									maximumTemplates: 64,
									maximumPlanTargets: 512
								})
							})
						}
					})
				};
			}
			return {
				case: 'configurationResult',
				value: create(ConfigurationResultSchema, {
					result: {
						case: 'plan',
						value: create(ConfigurationPlanSchema, {
							planId: 'plan-1',
							configurationRevision: 'revision-4',
							valid: true
						})
					}
				})
			};
		};
		const client = new ConfigurationControlClient(send);

		const snapshot = await client.getSnapshot();
		expect(snapshot).toMatchObject({
			contract_version: 1,
			configuration_revision: 'revision-4',
			defaults: {
				configured_backend: 'reo-proto',
				effective_backend: 'reo-proto'
			}
		});

		await client.plan({
			expected_configuration_revision: snapshot.configuration_revision,
			targets: { mode: 'camera-ids', camera_ids: ['192.0.2.10'] },
			change: { mode: 'patch', patch: { backend: { operation: 'clear' } } }
		});

		const request = commands[1];
		expect(request?.case).toBe('configurationCommand');
		if (request?.case !== 'configurationCommand') throw new Error('configuration request missing');
		expect(request.value.action.case).toBe('plan');
		if (request.value.action.case !== 'plan') throw new Error('plan action missing');
		expect(request.value.action.value.targets?.selection.case).toBe('cameraIds');
		expect(request.value.action.value.change?.change.case).toBe('patch');
		if (request.value.action.value.change?.change.case !== 'patch') {
			throw new Error('camera patch missing');
		}
		expect(request.value.action.value.change.change.value.backend?.value).toEqual({
			case: 'clear',
			value: true
		});
	});

	it('fails closed on an unsupported snapshot contract version', async () => {
		const client = new ConfigurationControlClient(async () => ({
			case: 'configurationResult',
			value: create(ConfigurationResultSchema, {
				result: {
					case: 'snapshot',
					value: create(ConfigurationSnapshotSchema, {
						contractVersion: 2,
						configurationRevision: 'revision-future',
						defaults: create(CameraDefaultValuesSchema),
						limits: create(ConfigurationLimitsSchema)
					})
				}
			})
		}));

		await expect(client.getSnapshot()).rejects.toThrow(
			'unsupported configuration contract version 2'
		);
	});

	it('assembles every snapshot page and sends the server page token', async () => {
		const pageTokens: string[] = [];
		const defaults = create(CameraDefaultValuesSchema, {
			effectiveBackend: ProtoCameraBackend.AUTO
		});
		const limits = create(ConfigurationLimitsSchema, { maximumPlanTargets: 64 });
		const client = new ConfigurationControlClient(async (command) => {
			if (command.case !== 'configurationCommand' || command.value.action.case !== 'get') {
				throw new Error('expected configuration snapshot request');
			}
			const pageToken = command.value.action.value.pageToken;
			pageTokens.push(pageToken);
			return {
				case: 'configurationResult',
				value: create(ConfigurationResultSchema, {
					result: {
						case: 'snapshot',
						value: create(ConfigurationSnapshotSchema, {
							contractVersion: 1,
							configurationRevision: 'revision-pages',
							defaults,
							limits,
							totalCameraCount: 0,
							nextPageToken: pageToken ? '' : 'revision-pages:0'
						})
					}
				})
			};
		});

		await expect(client.getSnapshot()).resolves.toMatchObject({
			configuration_revision: 'revision-pages',
			cameras: []
		});
		expect(pageTokens).toEqual(['', 'revision-pages:0']);
	});

	it('rejects mixed revisions while snapshot pages are loading', async () => {
		let requestCount = 0;
		const client = new ConfigurationControlClient(async () => {
			requestCount += 1;
			return {
				case: 'configurationResult',
				value: create(ConfigurationResultSchema, {
					result: {
						case: 'snapshot',
						value: create(ConfigurationSnapshotSchema, {
							contractVersion: 1,
							configurationRevision: requestCount === 1 ? 'revision-a' : 'revision-b',
							defaults: create(CameraDefaultValuesSchema),
							limits: create(ConfigurationLimitsSchema),
							nextPageToken: requestCount === 1 ? 'revision-a:1' : ''
						})
					}
				})
			};
		});

		await expect(client.getSnapshot()).rejects.toThrow(
			'Configuration changed while snapshot pages were loading.'
		);
	});

	it('loads the complete current snapshot after a compact apply response', async () => {
		const actions: string[] = [];
		const client = new ConfigurationControlClient(async (command) => {
			if (command.case !== 'configurationCommand' || !command.value.action.case) {
				throw new Error('expected configuration command');
			}
			actions.push(command.value.action.case);
			if (command.value.action.case === 'apply') {
				return {
					case: 'configurationResult',
					value: create(ConfigurationResultSchema, {
						result: {
							case: 'applied',
							value: create(ConfigurationApplyResultSchema, {
								planId: 'plan-1',
								configurationCommitted: true
							})
						}
					})
				};
			}
			return {
				case: 'configurationResult',
				value: create(ConfigurationResultSchema, {
					result: {
						case: 'snapshot',
						value: create(ConfigurationSnapshotSchema, {
							contractVersion: 1,
							configurationRevision: 'revision-after-apply',
							defaults: create(CameraDefaultValuesSchema),
							limits: create(ConfigurationLimitsSchema)
						})
					}
				})
			};
		});

		await expect(client.apply('plan-1', 'revision-before-apply')).resolves.toMatchObject({
			configuration_committed: true,
			snapshot: { configuration_revision: 'revision-after-apply' }
		});
		expect(actions).toEqual(['apply', 'get']);
	});
});
