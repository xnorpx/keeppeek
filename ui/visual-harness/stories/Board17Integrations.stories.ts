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
					'Board 17 integration architectures rendered from the production evidence owner, including MQTT 5 configuration and broker health.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '2UB-0',
			frameId: '2UL-0',
			scenarioId: 'settings.desktop.integrations',
			reference: 'references/17-integrations.png',
			referenceSha256: 'a78ebb32d7b258797f121e129c24ef084314979a9ddd7e4eab66969c74e6a508',
			exceptions: [
				'HealthCommand, /logs, /metrics, exact-origin CORS, and Bearer access exist, but they do not prove an external integration is configured.',
				'The Home Assistant card package, per-key token scope registry, origin editor, and token rotation command are unavailable.',
				'Webhook registry/retry runtime, Prometheus scrape configuration, and external collector health evidence remain unavailable.'
			]
		}
	}
} satisfies Meta<typeof Board17IntegrationsStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const OperationalBoundaries: Story = {
	name: 'Operational boundaries'
};
