import type { Meta, StoryObj } from '@storybook/svelte';
import { configZipDemo } from '../demo-scenarios-ops';
import DemoConfigZipStory from './DemoConfigZipStory.svelte';

const meta = {
	title: 'Demos/Config Zip',
	component: DemoConfigZipStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records exporting a format-3 configuration ZIP, gating local apply behind explicit confirmation, staging until restart, and rejecting a modified archive without modifying live files.'
			}
		}
	}
} satisfies Meta<typeof DemoConfigZipStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ExportStageAndReject: Story = {
	parameters: {
		paper: configZipDemo.metadata.paper,
		demo: configZipDemo.metadata.demo
	}
};
