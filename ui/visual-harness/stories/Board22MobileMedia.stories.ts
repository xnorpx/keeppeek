import type { Meta, StoryObj } from '@storybook/svelte';
import Board22MobileMediaStory from './Board22MobileMediaStory.svelte';

const meta = {
	title: 'Mobile/Peek Keep Events',
	component: Board22MobileMediaStory,
	parameters: { viewport: { defaultViewport: 'reset' } }
} satisfies Meta<typeof Board22MobileMediaStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Peek: Story = {
	args: { state: 'peek' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '3FF-0',
			frameId: '3FQ-0',
			scenarioId: 'peek.mobile.live',
			reference: 'references/22-mobile-peek.png',
			referenceSha256: 'bdeca9ecc8b6aa34636ac474eb7d32d1a90481ff089cafb1080ead60e3a6af7a'
		}
	}
};

export const Keep: Story = {
	args: { state: 'keep' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '3FF-0',
			frameId: '3HN-0',
			scenarioId: 'keep.mobile.timeline',
			reference: 'references/22-mobile-keep.png',
			referenceSha256: 'f32ef096bee42369b752ec9a6ab8b55ae5eb26931b94aee199d6dbb81e154c5d'
		}
	}
};

export const Events: Story = {
	args: { state: 'events' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '3FF-0',
			frameId: '3K0-0',
			scenarioId: 'events.mobile.browse',
			reference: 'references/22-mobile-events.png',
			referenceSha256: 'c6b2868d5f1dcc4f4d368ffb987427f04a12846beb5e6ae79c998450c2bd2c73'
		}
	}
};
