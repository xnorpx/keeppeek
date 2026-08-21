import type { Meta, StoryObj } from '@storybook/svelte';
import LightThemePeekStory from './LightThemePeekStory.svelte';

const meta = {
	title: 'Peek/Light Theme',
	component: LightThemePeekStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 34 light-theme shell rendered with the production Peek camera tile and deterministic mixed-health fixtures.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '5SM-0',
			frameId: '5SZ-0',
			scenarioId: 'peek.desktop.light-theme',
			reference: 'references/34-light-theme-peek.png',
			referenceSha256: '55828067fdc8a63cc17572a29b597ec1559ad18bd412b3222d9090cf2c73819f'
		}
	}
} satisfies Meta<typeof LightThemePeekStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const LiveWall: Story = {
	name: 'Mixed health live wall'
};
