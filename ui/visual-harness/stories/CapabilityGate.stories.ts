import type { Meta, StoryObj } from '@storybook/svelte';
import CapabilityGateStory from './CapabilityGateStory.svelte';

const meta = {
	title: 'Foundation/Capability Gate',
	component: CapabilityGateStory,
	tags: ['autodocs'],
	args: {
		action: 'Export clip',
		capability: 'keeppeek.media-export.v1',
		supported: false
	},
	parameters: {
		docs: {
			description: {
				component:
					'Fail-closed replacement for backend-owned commands whose exact server capability is unavailable.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '40K-0'
		}
	}
} satisfies Meta<typeof CapabilityGateStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Missing: Story = {
	name: 'Missing — export clip',
	parameters: {
		docs: {
			description: {
				story: 'The command is replaced by its verb and the exact required server capability.'
			}
		},
		paper: { scenarioId: 'foundation.capability-gate.missing' }
	}
};

export const Ready: Story = {
	name: 'Ready — export clip',
	args: { supported: true },
	parameters: {
		docs: {
			description: {
				story: 'The production command renders only when the exact capability is advertised.'
			}
		},
		paper: { scenarioId: 'foundation.capability-gate.ready' }
	}
};
