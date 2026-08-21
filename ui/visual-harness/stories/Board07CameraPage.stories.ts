import type { Meta, StoryObj } from '@storybook/svelte';
import Board07CameraPageStory from './Board07CameraPageStory.svelte';

const meta = {
	title: 'Cameras/Camera Page',
	component: Board07CameraPageStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 7 one-camera desktop page with shared WebRTC PTZ controls and evidence-safe profile, health, connection, event, audio, and advanced sections.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: 'TS-0',
			frameId: 'U2-0',
			scenarioId: 'camera.desktop.details-ptz',
			reference: 'references/07-camera-details-ptz.png',
			referenceSha256: 'b2242878797afd3b47cd81202f6d56404d78da36330d1cef233430172931a5c5',
			exceptions: [
				'Per-camera retention, recording mode, inheritance, broad save, preset mutation, and audio commands are not returned or implemented.',
				'Event records expose source categories rather than Paper’s publisher registry, tokens, mappings, heartbeats, and counts.',
				'Deterministic media pixels and live recording status are omitted; PTZ movement and preset recall remain on canonical WebRTC control.'
			]
		}
	}
} satisfies Meta<typeof Board07CameraPageStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const DetailsAndPtz: Story = {};
