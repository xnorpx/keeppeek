import type { Meta, StoryObj } from '@storybook/svelte';
import Board25MobileAddCameraStory from './Board25MobileAddCameraStory.svelte';

const meta = {
	title: 'Cameras/Mobile Add Camera',
	component: Board25MobileAddCameraStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 25 three-stage mobile wizard rendered from the production draft owner. Configuration writes remain exclusive to the final Review & Save action.'
			}
		}
	}
} satisfies Meta<typeof Board25MobileAddCameraStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const FindAndConnect: Story = {
	name: 'Find and connect',
	args: { stage: 'find-connect' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '40H-0',
			frameId: '481-0',
			scenarioId: 'cameras.mobile.add-wizard',
			reference: 'references/25-mobile-find-connect.png',
			referenceSha256: '310a61ceca39b8b98071baf2a6449caf8f45ae8d8db7b4abf69969d7f8711214'
		}
	}
};

export const Streams: Story = {
	name: 'Stream declarations',
	args: { stage: 'streams' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '40H-0',
			frameId: '49K-0',
			scenarioId: 'cameras.mobile.add-streams',
			reference: 'references/25-mobile-streams.png',
			referenceSha256: '8d9dfc99c3c4c0e51ca6c79d2294e61fadddff0eed916ae66865a7f74f672f97'
		}
	}
};

export const Review: Story = {
	name: 'Review and save',
	args: { stage: 'review' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '40H-0',
			frameId: '4B5-0',
			scenarioId: 'cameras.mobile.add-review',
			reference: 'references/25-mobile-review.png',
			referenceSha256: '35656121bd00c8372eae00bc380ca072c84e43dfb9db57f71ffaa6fa23154cc8'
		}
	}
};
