import type { Meta, StoryObj } from '@storybook/svelte';
import Board33StatesStory from './Board33StatesStory.svelte';

const meta = {
	title: 'States/Waiting and Empty',
	component: Board33StatesStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 33 waiting, empty, applying, and lane-preserving skeleton states rendered from their production Svelte owners.'
			}
		}
	}
} satisfies Meta<typeof Board33StatesStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const FirstKeyframe: Story = {
	args: { state: 'first-keyframe' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '5P3-0',
			frameId: '5PL-0',
			scenarioId: 'peek.waiting.first-keyframe',
			reference: 'references/33-first-keyframe.png',
			referenceSha256: '8bec87079b9d92f34c1f64145e9bf9a05b2645e09ca4d9014b5d377c2adcd329'
		}
	}
};

export const ColdSeek: Story = {
	args: { state: 'cold-seek' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '5P3-0',
			frameId: '5Q0-0',
			scenarioId: 'keep.waiting.cold-seek',
			reference: 'references/33-cold-seek.png',
			referenceSha256: '0ce048f7dcdf3f37aac088a59fe810b68be262ec4537e7fd19a889603134b8f2',
			exceptions: ['The production route omits a storage tier until the server reports one.']
		}
	}
};

export const Discovery: Story = {
	args: { state: 'discovery' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '5P3-0',
			frameId: '5QA-0',
			scenarioId: 'cameras.waiting.discovery',
			reference: 'references/33-discovery-progress.png',
			referenceSha256: 'c1a6cd81daa61172fbffa12afd77f43be22e9a523aac36ad6eb4c8cb5b9aeb67',
			exceptions: [
				'The production route omits probe counts because discovery returns only final results.'
			]
		}
	}
};

export const NoResults: Story = {
	args: { state: 'no-results' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '5P3-0',
			frameId: '5QR-0',
			scenarioId: 'events.empty.no-results',
			reference: 'references/33-no-results.png',
			referenceSha256: 'f01b069be69ee0d3f907b6cc2af969f72ac3d0be9cb11aa4ec994062aa6f850e'
		}
	}
};

export const Applying: Story = {
	args: { state: 'applying' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '5P3-0',
			frameId: '5RE-0',
			scenarioId: 'settings.waiting.applying',
			reference: 'references/33-settings-applying.png',
			referenceSha256: 'e2833d754b32c64e9d28cf0c6faca4a852f62335f3d0b3d0689271a85de94e17'
		}
	}
};

export const FleetSkeleton: Story = {
	args: { state: 'fleet-skeleton' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '5P3-0',
			frameId: '5RW-0',
			scenarioId: 'cameras.waiting.fleet-skeleton',
			reference: 'references/33-fleet-skeleton.png',
			referenceSha256: '5e6a6eb745f310ff04245a30503e91a1273d9a110dbb8639a7bd152895a4c4df'
		}
	}
};
