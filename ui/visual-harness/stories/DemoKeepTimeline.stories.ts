import type { Meta, StoryObj } from '@storybook/svelte';
import { keepTimelineExportDemo } from '../demo-scenarios-watch';
import DemoKeepTimelineStory from './DemoKeepTimelineStory.svelte';

const meta = {
	title: 'Demos/Keep Timeline',
	component: DemoKeepTimelineStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records Keep Timeline UTC event scrubbing, Stories and Footage calendar, multi-camera Swimlanes, evidence MP4 export with SHA-256 checksum, and bounded silent Timelapse generation.'
			}
		}
	}
} satisfies Meta<typeof DemoKeepTimelineStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ReviewAndExport: Story = {
	parameters: {
		paper: keepTimelineExportDemo.metadata.paper,
		demo: keepTimelineExportDemo.metadata.demo
	}
};
