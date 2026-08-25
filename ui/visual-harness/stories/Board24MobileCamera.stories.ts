import type { Meta, StoryObj } from '@storybook/svelte';
import Board24MobileCameraStory from './Board24MobileCameraStory.svelte';

const meta = {
	title: 'Cameras/Mobile Camera',
	component: Board24MobileCameraStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 24 mobile camera modes rendered from the production single-peer camera owner and shared WebRTC PTZ controls.'
			}
		}
	}
} satisfies Meta<typeof Board24MobileCameraStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Live: Story = {
	name: 'Live camera',
	args: { mode: 'live' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '40G-0',
			frameId: '41Z-0',
			scenarioId: 'camera.mobile.details-ptz',
			reference: 'references/24-mobile-camera-live.png',
			referenceSha256: '403b8fa83523eee07d1ec979e6eb0c431b332ea134840937d41dda96d79e5f9c'
		}
	}
};

export const Ptz: Story = {
	name: 'PTZ control',
	args: { mode: 'ptz' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '40G-0',
			frameId: '42R-0',
			scenarioId: 'camera.mobile.ptz',
			reference: 'references/24-mobile-camera-ptz.png',
			referenceSha256: '8d40405a302fbbe234eca8120f27af63183f7cca18d37d52e15bb7dd97aa4ada'
		}
	}
};

export const Settings: Story = {
	name: 'Camera settings evidence',
	args: { mode: 'settings' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '40G-0',
			frameId: '43E-0',
			scenarioId: 'camera.mobile.settings',
			reference: 'references/24-mobile-camera-settings.png',
			referenceSha256: 'e6ac9f8903530b1d82903b6665763b85a3239f69da32d1f91b3d4dd5e0416215'
		}
	}
};
