import type { Meta, StoryObj } from '@storybook/svelte';
import Board31HistoryStory from './Board31HistoryStory.svelte';

const meta = {
	title: 'Peek/Focus History',
	component: Board31HistoryStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 31 focused-live and Keep-history states rendered from the production History link.'
			}
		}
	}
} satisfies Meta<typeof Board31HistoryStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const FocusedHistory: Story = {
	args: { state: 'focused' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '5GD-0',
			frameId: '5GV-0',
			scenarioId: 'peek.desktop.focus-history',
			reference: 'references/31-focus-history.png',
			referenceSha256: 'c74c9dbaa63ac13c1434ccba12ec88486155b9c923312d4705594d87a93627fb'
		}
	}
};

export const HistoryKeep: Story = {
	args: { state: 'keep' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '5GD-0',
			frameId: '5I0-0',
			scenarioId: 'peek.desktop.history-keep',
			reference: 'references/31-history-keep.png',
			referenceSha256: '35e508cdec62a71892a6dc84aed516c9889a40bda39b4f97965dbd56dd4b9092'
		}
	}
};
