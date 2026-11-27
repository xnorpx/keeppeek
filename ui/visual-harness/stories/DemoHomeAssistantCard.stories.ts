import type { Meta, StoryObj } from '@storybook/svelte';
import { homeAssistantCardDemo } from '../demo-scenarios-integrations';
import DemoHomeAssistantCardStory from './DemoHomeAssistantCardStory.svelte';

const meta = {
	title: 'Demos/Home Assistant',
	component: DemoHomeAssistantCardStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records the Home Assistant Lovelace card and visual editor discovering sources, updating columns and layout live, focusing a camera tile, and returning to the grid.'
			}
		}
	}
} satisfies Meta<typeof DemoHomeAssistantCardStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const CardAndEditor: Story = {
	parameters: {
		paper: homeAssistantCardDemo.metadata.paper,
		demo: homeAssistantCardDemo.metadata.demo
	}
};
