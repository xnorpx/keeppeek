import type { Meta, StoryObj } from '@storybook/svelte';
import Board23CameraDefaultsStory from './Board23CameraDefaultsStory.svelte';

const meta = {
	title: 'Settings/Camera Defaults',
	component: Board23CameraDefaultsStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 23 Camera Defaults content rendered from the production evidence-safe section and deterministic per-camera configuration records.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '3VQ-0',
			frameId: '3X7-0',
			scenarioId: 'settings.desktop.camera-defaults',
			reference: 'references/23-camera-defaults-content.png',
			referenceSha256: '373aa28cc59e1ff1db5ab03a06ff91b4f0ca8c8e3b08ee02b4edcfdc9f3a604f'
		}
	}
} satisfies Meta<typeof Board23CameraDefaultsStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Evidence: Story = {
	name: 'Per-camera evidence'
};
