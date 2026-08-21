export type IntegrationEvidence = {
	id: 'home-assistant' | 'mqtt-forwarder' | 'webhooks' | 'prometheus';
	label: string;
	architecture: string;
	egress: string;
	configurationRuntime: 'unavailable';
	healthRuntime: 'unavailable';
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
		label: 'MQTT event forwarder',
		architecture: 'Separate durable service subscribes to events and publishes to a broker.',
		egress: 'Events and selected attachments would leave through the forwarder.',
		configurationRuntime: 'unavailable',
		healthRuntime: 'unavailable',
		implementedEndpoint: null,
		prerequisites: ['event subscription runtime', 'stored-event backfill', 'forwarder binary']
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
