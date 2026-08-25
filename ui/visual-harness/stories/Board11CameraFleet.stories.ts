import type { Meta, StoryObj } from '@storybook/svelte';
import Board11CameraFleetStory from './Board11CameraFleetStory.svelte';

const meta = {
	title: 'Cameras/Fleet',
	component: Board11CameraFleetStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 11 mixed camera fleet rendered from the shared production 56px row and the same presentation model used by the bounded 127-source virtualizer.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '1NF-0',
			frameId: '1NP-0',
			scenarioId: 'cameras.desktop.fleet',
			reference: 'references/11-camera-fleet.png',
			referenceSha256: 'dd3710b7bfadca376ca34cb086096f186339db5e5e964ce77771f34d42c312b9',
			exceptions: [
				'Fleet and health snapshots do not expose last-event timestamp, kind, or publisher provenance.',
				'Group membership/directory state and service-published media variants are unavailable.',
				'Bulk set-recording, move-to-group, restart-stream, and remove operations have no implemented shared command contract.'
			]
		}
	}
} satisfies Meta<typeof Board11CameraFleetStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const MixedSources: Story = {
	name: 'Mixed sources'
};
