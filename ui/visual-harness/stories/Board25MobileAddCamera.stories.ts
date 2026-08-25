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
			referenceSha256: '09a558f2bb7c33e63f5673c4fcc05368da95aafbd358f2bc89a6a171f9c46a92'
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
			referenceSha256: '3f95c15b81dd2df6446bb9c800b1600ed49551800561490982c5a73846f00f73'
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
			referenceSha256: 'a35256cf3ff08270f819588a49c8816e2d4af10ec21fef28dcf4ce78cd22138b'
		}
	}
};
