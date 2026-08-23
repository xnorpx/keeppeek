import type { Meta, StoryObj } from '@storybook/svelte';
import { board31HistoryDemo } from '../demo-scenarios';
import DemoHistoryStory from './DemoHistoryStory.svelte';

const meta = {
	title: 'Demos/Peek History',
	component: DemoHistoryStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records the focused History action and its deterministic Keep landing using the typed demo action timeline.'
			}
		}
	}
} satisfies Meta<typeof DemoHistoryStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const OpenKeepHistory: Story = {
	parameters: {
		paper: board31HistoryDemo.metadata.paper,
		demo: board31HistoryDemo.metadata.demo
	}
};
