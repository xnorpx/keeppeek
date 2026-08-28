import { create, type JsonObject } from '@bufbuild/protobuf';
import { describe, expect, it } from 'vitest';
import { MqttControlClient } from './control-client-mqtt';
import { StateEntrySchema, StateStoreResultSchema, type Ok, type Request } from './proto/webrtc_pb';
import type { MqttIntegration, MqttSettingsUpdate, MqttTestResult } from './integrations';

const integration: MqttIntegration = {
	configuration: {
		enabled: true,
		broker_url: 'mqtts://broker.example:8883',
		client_id: 'keeppeek',
		instance_id: 'home-nvr',
		forwarder_id: 'mqtt',
		topic_prefix: 'keeppeek',
		username: 'operator',
		password_configured: true,
		tls_ca_path: '/etc/keeppeek/mqtt-ca.pem',
		qos: 1,
		retain_events: false,
		retain_health: true,
		outbox_max_mb: 64,
		retry_min_ms: 250,
		retry_max_ms: 30_000
	},
	status: {
		enabled: true,
		state: 'connected',
		detail: 'MQTT 5 broker is connected.',
		connected_at_ms: 1,
		last_received_at_ms: 2,
		last_delivered_at_ms: 3,
		pending_items: 0,
		pending_bytes: 0,
		oldest_unacknowledged_timestamp_ms: null,
		retry_count: 0,
		duplicate_count: 0,
		outbox_limit_bytes: 67_108_864
	},
	configuration_revision: '7'
};

function stateResult(
	key: string,
	schema: string,
	value: JsonObject,
	revision = 7n
): NonNullable<Ok['result']> {
	return {
		case: 'stateStoreResult',
		value: create(StateStoreResultSchema, {
			result: {
				case: 'entry',
				value: create(StateEntrySchema, {
					namespace: 'keeppeek.integrations.mqtt',
					key,
					schema,
					value,
					revision,
					ownerId: 'server'
				})
			}
		})
	};
}

describe('MQTT control client', () => {
	it('gets, updates, and tests MQTT through StateStore without reflecting a password', async () => {
		const { configuration_revision: _revision, ...state } = integration;
		const testResult: MqttTestResult = { ok: true, kind: null, detail: 'Connected.' };
		const responses: NonNullable<Ok['result']>[] = [
			stateResult('configuration', 'keeppeek.mqtt-configuration.v1', state, 7n),
			stateResult('configuration', 'keeppeek.mqtt-configuration.v1', state, 8n),
			stateResult('test', 'keeppeek.mqtt-test-result.v1', testResult, 8n)
		];
		const sent: Request['command'][] = [];
		const client = new MqttControlClient(async (command) => {
			sent.push(command);
			return responses.shift()!;
		});
		const update: MqttSettingsUpdate = {
			...integration.configuration,
			password: 'write-only-secret',
			expected_configuration_revision: '7'
		};

		await expect(client.get()).resolves.toEqual(integration);
		const saved = await client.update(update);
		expect(saved).toMatchObject({ configuration_revision: '8' });
		await expect(client.test(update)).resolves.toEqual(testResult);

		expect(sent.map((command) => command.case)).toEqual([
			'stateStoreCommand',
			'stateStoreCommand',
			'stateStoreCommand'
		]);
		const updateCommand = sent[1];
		expect(updateCommand.case).toBe('stateStoreCommand');
		if (updateCommand.case !== 'stateStoreCommand' || updateCommand.value.action.case !== 'put') {
			throw new Error('expected MQTT StateStore put command');
		}
		expect(updateCommand.value.action.value).toMatchObject({
			namespace: 'keeppeek.integrations.mqtt',
			key: 'configuration',
			schema: 'keeppeek.mqtt-configuration.v1',
			expectedRevision: 7n
		});
		expect(updateCommand.value.action.value.value?.password).toBe('write-only-secret');
		expect(JSON.stringify(saved)).not.toContain('write-only-secret');
	});
});
