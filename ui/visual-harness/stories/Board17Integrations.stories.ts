import type { Meta, StoryObj } from '@storybook/svelte';
import Board17IntegrationsStory from './Board17IntegrationsStory.svelte';

const meta = {
	title: 'Settings/Integrations',
	component: Board17IntegrationsStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 17 integration architectures rendered from the production evidence owner, separating implemented operational endpoints from unavailable third-party configuration and health.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '2UB-0',
			frameId: '2UL-0',
			scenarioId: 'settings.desktop.integrations',
			reference: 'references/17-integrations.png',
			referenceSha256: 'bdd8ccd79fcebc11497e7b71a9259c2ba6a7e8f45f7940c90e29bcfc868af8f1',
			exceptions: [
				'HealthCommand, /logs, /metrics, exact-origin CORS, and Bearer access exist, but they do not prove an external integration is configured.',
				'The Home Assistant card package, per-key token scope registry, origin editor, and token rotation command are unavailable.',
				'MQTT forwarder, webhook registry/retry runtime, Prometheus scrape configuration, and external health evidence are unavailable.'
			]
		}
	}
} satisfies Meta<typeof Board17IntegrationsStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const OperationalBoundaries: Story = {
	name: 'Operational boundaries'
};
