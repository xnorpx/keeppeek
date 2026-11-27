import type { Meta, StoryObj } from '@storybook/svelte';
import { firstRunDemo } from '../demo-scenarios-onboarding';
import DemoFirstRunStory from './DemoFirstRunStory.svelte';

const meta = {
	title: 'Demos/First Run',
	component: DemoFirstRunStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records the guided first-run setup path, including storage write verification, one-time initial Administrator key retrieval, and transition to the empty-fleet workspace.'
			}
		}
	}
} satisfies Meta<typeof DemoFirstRunStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const GuidedSetup: Story = {
	parameters: {
		paper: firstRunDemo.metadata.paper,
		demo: firstRunDemo.metadata.demo
	}
};
