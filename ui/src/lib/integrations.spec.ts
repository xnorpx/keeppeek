import { describe, expect, it } from 'vitest';
import { integrationsEvidence } from '$lib/integrations';

describe('integration evidence', () => {
	it('exposes only the implemented MQTT configuration and health runtime', () => {
		const evidence = integrationsEvidence();

		expect(evidence.connectedCount).toBeNull();
		expect(evidence.configuredCount).toBeNull();
		expect(evidence.integrations).toHaveLength(4);
		expect(
			evidence.integrations.find((integration) => integration.id === 'mqtt-forwarder')
		).toMatchObject({
			configurationRuntime: 'available',
			healthRuntime: 'available',
			implementedEndpoint: '/integrations/mqtt'
		});
		expect(
			evidence.integrations
				.filter((integration) => !['mqtt-forwarder', 'prometheus'].includes(integration.id))
				.every(
					(integration) =>
						integration.configurationRuntime === 'unavailable' &&
						integration.healthRuntime === 'unavailable'
				)
		).toBe(true);
	});

	it('preserves direct-card and pull-metrics egress boundaries', () => {
		const integrations = integrationsEvidence().integrations;

		expect(integrations.find((integration) => integration.id === 'home-assistant')).toMatchObject({
			architecture: 'Direct browser card; Home Assistant is not a media proxy.'
		});
		expect(integrations.find((integration) => integration.id === 'prometheus')).toMatchObject({
			egress: 'No push. A remote collector would read metrics from KeepPeek.',
			implementedEndpoint: '/metrics'
		});
		expect(integrationsEvidence().thirdPartyMediaRelay).toBe(false);
	});

	it('names only implemented operational evidence as available', () => {
		expect(integrationsEvidence().availableOperationalEvidence).toEqual([
			'HealthCommand',
			'/logs',
			'/metrics'
		]);
	});
});
