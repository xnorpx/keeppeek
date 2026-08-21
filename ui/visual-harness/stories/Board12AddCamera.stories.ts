import type { Meta, StoryObj } from '@storybook/svelte';
import Board12AddCameraStory from './Board12AddCameraStory.svelte';

const meta = {
	title: 'Cameras/Add Camera Desktop',
	component: Board12AddCameraStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 12 desktop stream-role step rendered from the production five-step camera draft owner, preserving the first-write-at-save boundary.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '1VG-0',
			frameId: '1VQ-0',
			scenarioId: 'cameras.desktop.add-wizard',
			reference: 'references/12-add-camera-stream-evidence.png',
			referenceSha256: '55b57aad24ed47de14f0fd0e86ea5dc525a6d26baf10c55ab0dc68c0a6168e6c',
			exceptions: [
				'The server has no candidate-camera authentication or stream-probe command, so URLs remain declarations rather than decoded proof.',
				'Audio codec, first-keyframe timing, frame count, bitrate, recording mode, retention impact, and group assignment are unavailable before save.',
				'A permanent source ID is assigned only after the final configuration write; credentials remain write-only draft data.'
			]
		}
	}
} satisfies Meta<typeof Board12AddCameraStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const StreamDeclarations: Story = {
	name: 'Stream declarations'
};
