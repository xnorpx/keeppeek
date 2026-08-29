import { create, type JsonObject } from '@bufbuild/protobuf';
import {
	GetStateSchema,
	PutStateSchema,
	StateStoreCommandSchema,
	type Ok,
	type Request,
	type StateEntry
} from './proto/webrtc_pb';
import type { MqttIntegration, MqttSettingsUpdate, MqttTestResult } from './integrations';

const namespace = 'keeppeek.integrations.mqtt';
const configurationKey = 'configuration';
const configurationSchema = 'keeppeek.mqtt-configuration.v1';
const testKey = 'test';
const testSchema = 'keeppeek.mqtt-test.v1';
const testResultSchema = 'keeppeek.mqtt-test-result.v1';

type SendRequest = (command: Request['command']) => Promise<Ok['result']>;

export class MqttControlClient {
	constructor(private readonly sendRequest: SendRequest) {}

	async get(): Promise<MqttIntegration> {
		const command = create(StateStoreCommandSchema, {
			action: {
				case: 'get',
				value: create(GetStateSchema, { namespace, key: configurationKey })
			}
		});
		return mqttIntegration(await this.entry(command), configurationSchema);
	}

	async update(update: MqttSettingsUpdate): Promise<MqttIntegration> {
		const command = create(StateStoreCommandSchema, {
			action: {
				case: 'put',
				value: create(PutStateSchema, {
					namespace,
					key: configurationKey,
					schema: configurationSchema,
					value: stateValue(update),
					expectedRevision: update.expected_configuration_revision
						? BigInt(update.expected_configuration_revision)
						: undefined
				})
			}
		});
		return mqttIntegration(await this.entry(command), configurationSchema);
	}

	async test(update: MqttSettingsUpdate): Promise<MqttTestResult> {
		const command = create(StateStoreCommandSchema, {
			action: {
				case: 'put',
				value: create(PutStateSchema, {
					namespace,
					key: testKey,
					schema: testSchema,
					value: stateValue(update)
				})
			}
		});
		const entry = await this.entry(command);
		if (entry.schema !== testResultSchema || !isMqttTestResult(entry.value)) {
			throw new Error('Server returned an invalid MQTT test result.');
		}
		return entry.value;
	}

	private async entry(
		command: ReturnType<typeof create<typeof StateStoreCommandSchema>>
	): Promise<StateEntry> {
		const result = await this.sendRequest({ case: 'stateStoreCommand', value: command });
		if (result.case !== 'stateStoreResult' || result.value.result.case !== 'entry') {
			throw new Error('Server returned an unexpected MQTT state response.');
		}
		const entry = result.value.result.value;
		if (entry.namespace !== namespace) {
			throw new Error('Server returned an unexpected MQTT state namespace.');
		}
		return entry;
	}
}

function stateValue(update: MqttSettingsUpdate): JsonObject {
	const { expected_configuration_revision: _revision, ...value } = update;
	return JSON.parse(JSON.stringify(value)) as JsonObject;
}

function mqttIntegration(entry: StateEntry, schema: string): MqttIntegration {
	if (entry.key !== configurationKey || entry.schema !== schema || !isMqttState(entry.value)) {
		throw new Error('Server returned invalid MQTT settings.');
	}
	return {
		...entry.value,
		configuration_revision: entry.revision.toString()
	};
}

function isMqttState(value: unknown): value is Omit<MqttIntegration, 'configuration_revision'> {
	if (!value || typeof value !== 'object') return false;
	const integration = value as Partial<MqttIntegration>;
	return isMqttConfiguration(integration.configuration) && isMqttStatus(integration.status);
}

function isMqttConfiguration(value: unknown): value is MqttIntegration['configuration'] {
	if (!value || typeof value !== 'object') return false;
	const config = value as Partial<MqttIntegration['configuration']>;
	return (
		typeof config.enabled === 'boolean' &&
		typeof config.broker_url === 'string' &&
		typeof config.client_id === 'string' &&
		typeof config.instance_id === 'string' &&
		typeof config.forwarder_id === 'string' &&
		typeof config.topic_prefix === 'string' &&
		(config.username === null || typeof config.username === 'string') &&
		typeof config.password_configured === 'boolean' &&
		(config.tls_ca_path === null || typeof config.tls_ca_path === 'string') &&
		typeof config.qos === 'number' &&
		typeof config.retain_events === 'boolean' &&
		typeof config.retain_health === 'boolean' &&
		typeof config.outbox_max_mb === 'number' &&
		typeof config.retry_min_ms === 'number' &&
		typeof config.retry_max_ms === 'number'
	);
}

function isMqttStatus(value: unknown): value is MqttIntegration['status'] {
	if (!value || typeof value !== 'object') return false;
	const status = value as Partial<MqttIntegration['status']>;
	return (
		['disabled', 'connecting', 'connected', 'degraded', 'outbox_full'].includes(
			status.state ?? ''
		) &&
		typeof status.enabled === 'boolean' &&
		typeof status.detail === 'string' &&
		typeof status.pending_items === 'number' &&
		typeof status.pending_bytes === 'number' &&
		typeof status.retry_count === 'number' &&
		typeof status.duplicate_count === 'number' &&
		typeof status.outbox_limit_bytes === 'number'
	);
}

function isMqttTestResult(value: unknown): value is MqttTestResult {
	if (!value || typeof value !== 'object') return false;
	const result = value as Partial<MqttTestResult>;
	return (
		typeof result.ok === 'boolean' &&
		typeof result.detail === 'string' &&
		(result.kind === null ||
			['authentication', 'tls', 'network', 'protocol', 'timeout'].includes(result.kind ?? ''))
	);
}
