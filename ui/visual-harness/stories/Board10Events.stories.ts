import type { Meta, StoryObj } from '@storybook/svelte';
import Board10EventsStory from './Board10EventsStory.svelte';

const meta = {
	title: 'Events/Desktop',
	component: Board10EventsStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 10 desktop Events browse and detail states rendered from the shared production Event card and drawer without synthesizing unavailable event evidence.'
			}
		}
	}
} satisfies Meta<typeof Board10EventsStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Browse: Story = {
	args: { state: 'browse' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '1GP-0',
			frameId: '1GZ-0',
			scenarioId: 'events.desktop.browse',
			reference: 'references/10-events-browse.png',
			referenceSha256: '5b3cd145334d1f7b014e8b3008f9db7ceb1b4254b7bf02c2e65ab9b692766eab',
			exceptions: [
				'The event API returns kind and source category but no narrative label or publisher service identity.',
				'Attachment history and frame counts are unavailable, so story cards expose only the returned kind.',
				'Neutral story media surfaces are candidates until approved deterministic Event imagery exists.'
			]
		}
	}
};

export const Detail: Story = {
	args: { state: 'detail' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '1GP-0',
			frameId: '1LM-0',
			scenarioId: 'events.desktop.detail',
			reference: 'references/10-event-detail.png',
			referenceSha256: '9e53e221f88e8a0cd3b5be1ab67ddc44e6a70fe113cfaf5939c0f9d92e2ed2a9',
			exceptions: [
				'Only one optional thumbnail URL is returned; multi-image attachment history is unavailable.',
				'Payload, revision history, and publisher source_id are not reported by the current event API.',
				'Export and bookmark controls remain fail-closed behind their exact unavailable capability IDs.'
			]
		}
	}
};
