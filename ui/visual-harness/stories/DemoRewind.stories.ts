import type { Meta, StoryObj } from '@storybook/svelte';
import { board31RewindDemo } from '../demo-scenarios';
import DemoRewindStory from './DemoRewindStory.svelte';

const meta = {
	title: 'Demos/Peek Rewind',
	component: DemoRewindStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records the production rewind control and its deterministic Keep landing using the typed demo action timeline.'
			}
		}
	}
} satisfies Meta<typeof DemoRewindStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const RewindOneCamera: Story = {
	parameters: {
		paper: board31RewindDemo.metadata.paper,
		demo: board31RewindDemo.metadata.demo
	}
};
