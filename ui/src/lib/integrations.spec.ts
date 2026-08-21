import { describe, expect, it } from 'vitest';
import { integrationsEvidence } from '$lib/integrations';

describe('integration evidence', () => {
	it('keeps every authored integration disconnected without runtime evidence', () => {
		const evidence = integrationsEvidence();

		expect(evidence.connectedCount).toBeNull();
		expect(evidence.configuredCount).toBeNull();
		expect(evidence.integrations).toHaveLength(4);
		expect(
			evidence.integrations
				.filter((integration) => integration.id !== 'prometheus')
				.every(
					(integration) =>
						integration.configurationRuntime === 'unavailable' &&
						integration.healthRuntime === 'unavailable' &&
						integration.implementedEndpoint === null
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
