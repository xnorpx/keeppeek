import type { Meta, StoryObj } from '@storybook/svelte';
import { eventsSearchReviewDemo } from '../demo-scenarios-watch';
import DemoEventsSearchStory from './DemoEventsSearchStory.svelte';

const meta = {
	title: 'Demos/Events Search',
	component: DemoEventsSearchStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records Events browsing, nested boolean filtering with EventExpressionBuilder, canonical preview and review/bookmark controls in EventDetailDrawer, and Activity summary trends, heatmap, and CSV export.'
			}
		}
	}
} satisfies Meta<typeof DemoEventsSearchStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const FilterDetailActivity: Story = {
	parameters: {
		paper: eventsSearchReviewDemo.metadata.paper,
		demo: eventsSearchReviewDemo.metadata.demo
	}
};
