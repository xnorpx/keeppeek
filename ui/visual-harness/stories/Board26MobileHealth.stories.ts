import type { Meta, StoryObj } from '@storybook/svelte';
import Board26MobileHealthStory from './Board26MobileHealthStory.svelte';

const meta = {
	title: 'Health/Mobile',
	component: Board26MobileHealthStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 26 mobile Health overview rendered from the production ranked-finding component and an API-shaped WebRTC health fixture.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '40I-0',
			frameId: '4CM-0',
			scenarioId: 'health.mobile.overview',
			reference: 'references/26-mobile-health-overview.png',
			referenceSha256: '2f05f294eaa5a7dff49d229d26f473d47e8a9666c0096cb21744a87882905ce7',
			exceptions: [
				'Mute 24h is omitted because the health control API exposes no issue-suppression command.'
			]
		}
	}
} satisfies Meta<typeof Board26MobileHealthStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Overview: Story = {
	name: 'Highest-cost issue first'
};
