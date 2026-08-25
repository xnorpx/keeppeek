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
			referenceSha256: '856fad797c1f0a4aa2897522249551e9a18cfdeaec4922b8506e022301d81201',
			exceptions: [
				'ONVIF candidate endpoints can be retrieved after sign-in, but URLs remain declarations rather than decoded proof until the camera is saved.',
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
