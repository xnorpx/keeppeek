export type IntegrationEvidence = {
	id: 'home-assistant' | 'mqtt-forwarder' | 'webhooks' | 'prometheus';
	label: string;
	architecture: string;
	egress: string;
	configurationRuntime: 'available' | 'unavailable';
	healthRuntime: 'available' | 'unavailable';
	implementedEndpoint: string | null;
	prerequisites: readonly string[];
};

const integrations = Object.freeze<IntegrationEvidence[]>([
	{
		id: 'home-assistant',
		label: 'Home Assistant',
		architecture: 'Direct browser card; Home Assistant is not a media proxy.',
		egress: 'A configured dashboard browser would connect directly to KeepPeek.',
		configurationRuntime: 'unavailable',
		healthRuntime: 'unavailable',
		implementedEndpoint: null,
		prerequisites: ['direct card package']
	},
	{
		id: 'mqtt-forwarder',
		label: 'MQTT 5 event forwarder',
		architecture: 'A supervised durable runtime publishes committed events to an MQTT 5 broker.',
		egress: 'Normalized event revisions and selected health changes leave through the forwarder.',
		configurationRuntime: 'available',
		healthRuntime: 'available',
		implementedEndpoint: '/integrations/mqtt',
		prerequisites: []
	},
	{
		id: 'webhooks',
		label: 'Webhooks',
		architecture: 'Server-owned signed event POST delivery with retry state.',
		egress: 'Configured event payloads would be pushed to each endpoint.',
		configurationRuntime: 'unavailable',
		healthRuntime: 'unavailable',
		implementedEndpoint: null,
		prerequisites: ['endpoint registry', 'signing secrets', 'durable retry queue']
	},
	{
		id: 'prometheus',
		label: 'Prometheus',
		architecture: 'A collector pulls text metrics; KeepPeek sends nothing proactively.',
		egress: 'No push. A remote collector would read metrics from KeepPeek.',
		configurationRuntime: 'unavailable',
		healthRuntime: 'unavailable',
		implementedEndpoint: '/metrics',
		prerequisites: ['scrape configuration UI', 'scrape health evidence']
	}
]);

export type IntegrationsEvidence = {
	integrations: readonly IntegrationEvidence[];
	connectedCount: null;
	configuredCount: null;
	availableOperationalEvidence: readonly ['HealthCommand', '/logs', '/metrics'];
	thirdPartyMediaRelay: false;
};

export function integrationsEvidence(): IntegrationsEvidence {
	return {
		integrations,
		connectedCount: null,
		configuredCount: null,
		availableOperationalEvidence: ['HealthCommand', '/logs', '/metrics'],
		thirdPartyMediaRelay: false
	};
}

export type MqttConnectionState =
	'disabled' | 'connecting' | 'connected' | 'degraded' | 'outbox_full';

export type MqttConfiguration = {
	enabled: boolean;
	broker_url: string;
	client_id: string;
	instance_id: string;
	forwarder_id: string;
	topic_prefix: string;
	username: string | null;
	password_configured: boolean;
	tls_ca_path: string | null;
	qos: number;
	retain_events: boolean;
	retain_health: boolean;
	outbox_max_mb: number;
	retry_min_ms: number;
	retry_max_ms: number;
};

export type MqttStatus = {
	enabled: boolean;
	state: MqttConnectionState;
	detail: string;
	connected_at_ms: number | null;
	last_received_at_ms: number | null;
	last_delivered_at_ms: number | null;
	pending_items: number;
	pending_bytes: number;
	oldest_unacknowledged_timestamp_ms: number | null;
	retry_count: number;
	duplicate_count: number;
	outbox_limit_bytes: number;
};

export type MqttIntegration = {
	configuration: MqttConfiguration;
	status: MqttStatus;
	configuration_revision: string;
};

export type MqttSettingsUpdate = Omit<MqttConfiguration, 'password_configured'> & {
	password?: string;
	clear_password?: boolean;
	expected_configuration_revision?: string;
};

export type MqttTestResult = {
	ok: boolean;
	kind: 'authentication' | 'tls' | 'network' | 'protocol' | 'timeout' | null;
	detail: string;
};
