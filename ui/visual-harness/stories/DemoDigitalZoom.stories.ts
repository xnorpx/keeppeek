import type { Meta, StoryObj } from '@storybook/svelte';
import { digitalZoomDemo } from '../demo-scenarios-watch';
import DemoDigitalZoomStory from './DemoDigitalZoomStory.svelte';

const meta = {
	title: 'Demos/Digital Zoom',
	component: DemoDigitalZoomStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records browser-side digital zoom from 1.0x to 4.0x, bounded pointer drag panning, 40 px arrow-key panning, and instant reset to 1.0x without moving physical PTZ.'
			}
		}
	}
} satisfies Meta<typeof DemoDigitalZoomStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const ZoomAndPan: Story = {
	parameters: {
		paper: digitalZoomDemo.metadata.paper,
		demo: digitalZoomDemo.metadata.demo
	}
};
