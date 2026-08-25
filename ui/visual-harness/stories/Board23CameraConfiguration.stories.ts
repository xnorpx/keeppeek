import type { Meta, StoryObj } from '@storybook/svelte';
import Board23CameraConfigurationStory from './Board23CameraConfigurationStory.svelte';

const meta = {
	title: 'Cameras/Configuration',
	component: Board23CameraConfigurationStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 23 renders the production per-camera configuration editor under the Camera page owner.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '3VQ-0',
			frameId: '9KT-0',
			scenarioId: 'camera.desktop.configuration',
			reference: 'references/23-camera-configuration.png',
			referenceSha256: 'b3cbcbd9860d504e8447a3c2d871d5e1f8582289ad1e59d5c820a55c75392f23'
		}
	}
} satisfies Meta<typeof Board23CameraConfigurationStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Editor: Story = {
	name: 'Per-camera editor'
};
