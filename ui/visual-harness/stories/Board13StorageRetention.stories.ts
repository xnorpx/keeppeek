import type { Meta, StoryObj } from '@storybook/svelte';
import Board13StorageRetentionStory from './Board13StorageRetentionStory.svelte';

const meta = {
	title: 'Settings/Storage and Retention',
	component: Board13StorageRetentionStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 13 measured disk capacity, configured storage tiers, paths, and fixed archive-pruning behavior rendered from the production evidence owner.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '233-0',
			frameId: '23D-0',
			scenarioId: 'settings.desktop.storage-retention',
			reference: 'references/13-storage-retention.png',
			referenceSha256: '888a17ae7c9ca723426274cc0bfb95df2bbd567ec5d4551bec04513abbdcb2b0',
			exceptions: [
				'Estimated retention is a configured-cap projection; actual oldest-footage time is not exposed.',
				'The active writer duration controls MP4 rollover and is not presented as a medium-tier age guarantee.',
				'Prune-oldest is fixed engine behavior, while selectable stop-on-full, per-camera retention, pins, and offsite locations are unavailable.'
			]
		}
	}
} satisfies Meta<typeof Board13StorageRetentionStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const MeasuredAndConfigured: Story = {
	name: 'Measured and configured'
};
