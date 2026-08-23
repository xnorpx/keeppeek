import type { Meta, StoryObj } from '@storybook/svelte';
import Board06PeekLiveWallStory from './Board06PeekLiveWallStory.svelte';

const meta = {
	title: 'Peek/Desktop Live Wall',
	component: Board06PeekLiveWallStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 6 fixed desktop shell and mixed live wall rendered from the production native-video Peek tile states.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: 'MX-0',
			frameId: 'N7-0',
			scenarioId: 'peek.desktop.live-wall',
			reference: 'references/06-peek-live-wall.png',
			referenceSha256: '669b362c076ac057a5bc625bd52170058104936e9f0a835c65245b8ab4e2342f',
			exceptions: [
				'The deterministic story uses neutral native-video surfaces and cannot become a media baseline without approved Paper imagery.',
				'Only six fixture cameras exist, so source pagination reports 6 of 6 instead of Paper’s unreturned 127-source total.',
				'Status metrics are deterministic health evidence; production playback remains separately covered with advancing native video frames.'
			]
		}
	}
} satisfies Meta<typeof Board06PeekLiveWallStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const MixedStates: Story = {};
