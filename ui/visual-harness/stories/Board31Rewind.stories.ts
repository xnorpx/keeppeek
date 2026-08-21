import type { Meta, StoryObj } from '@storybook/svelte';
import Board31RewindStory from './Board31RewindStory.svelte';

const meta = {
	title: 'Peek/Rewind to Keep',
	component: Board31RewindStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 31 live-ready and in-grid scrubbing states rendered from the production Peek rewind control and overlay.'
			}
		}
	}
} satisfies Meta<typeof Board31RewindStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Ready: Story = {
	args: { state: 'ready' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '5GD-0',
			frameId: '5GV-0',
			scenarioId: 'peek.desktop.rewind-ready',
			reference: 'references/31-rewind-ready.png',
			referenceSha256: 'e52c222cace55ecfdea6a9d67d5781fd7a2acd4696fb2372b5ca1255ae993f5b'
		}
	}
};

export const Scrubbing: Story = {
	args: { state: 'scrubbing' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '5GD-0',
			frameId: '5HF-0',
			scenarioId: 'peek.desktop.rewind-to-keep',
			reference: 'references/31-rewind-scrubbing.png',
			referenceSha256: '3e59cbc4a91f865e07fc75b741703b4e1df88926c06a73bdf60be038ae0e13ad'
		}
	}
};
