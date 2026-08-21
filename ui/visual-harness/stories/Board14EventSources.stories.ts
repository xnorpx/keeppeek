import type { Meta, StoryObj } from '@storybook/svelte';
import Board14EventSourcesStory from './Board14EventSourcesStory.svelte';

const meta = {
	title: 'Settings/Event Sources',
	component: Board14EventSourcesStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 14 catalog, persisted-origin, stored-field, and unavailable publisher-administration evidence rendered from the production owner.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '29O-0',
			frameId: '29Y-0',
			scenarioId: 'settings.desktop.event-sources',
			reference: 'references/14-event-sources.png',
			referenceSha256: '07b1e3f67e777af096a1265a8f8e836618ecd41678c14d686a1af22db875dcb4',
			exceptions: [
				'Catalog counts are all-time aggregates; events-today and last-event time are not reported.',
				'Persisted camera and keeppeek origins are categories, not publisher identities or sessions.',
				'Publisher registry, heartbeat, token metadata, scopes, permissions, type mappings, and WebRTC publication runtime are unavailable.'
			]
		}
	}
} satisfies Meta<typeof Board14EventSourcesStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const CatalogEvidence: Story = {
	name: 'Catalog evidence'
};
