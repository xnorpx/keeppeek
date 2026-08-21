import type { Meta, StoryObj } from '@storybook/svelte';
import ExportLifecycleStory from './ExportLifecycleStory.svelte';

const meta = {
	title: 'Keep/Export Lifecycle',
	component: ExportLifecycleStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 29 export states rendered from the production Svelte panel with deterministic WebRTC control fixtures.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '4VX-0',
			frameId: '4WA-0',
			scenarioId: 'keep.desktop.export-lifecycle',
			reference: 'references/29-export-job-lifecycle-states.png',
			referenceSha256: 'dde0bb83ff0ddb16a499bd5a88feb1af998c4a6f24838292e1dfe6b379a480e1',
			exceptions: [
				'Partial does not claim a reconnect cause because ExportJob exposes missing ranges, not a cause.',
				'Failed does not prescribe 200 MB because ExportJob exposes the server error, not required free space.'
			]
		}
	}
} satisfies Meta<typeof ExportLifecycleStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const States: Story = {
	name: 'Running, ready, partial, failed'
};
