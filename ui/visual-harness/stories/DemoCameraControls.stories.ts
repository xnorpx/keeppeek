import type { Meta, StoryObj } from '@storybook/svelte';
import { cameraControlsDemo } from '../demo-scenarios-onboarding';
import DemoCameraControlsStory from './DemoCameraControlsStory.svelte';

const meta = {
	title: 'Demos/Camera Controls',
	component: DemoCameraControlsStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records camera overview inspection, continuous PTZ steering and stop, preset recall, verified hardware motion-detection toggling, and Administrator browser Talk.'
			}
		}
	}
} satisfies Meta<typeof DemoCameraControlsStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const PtzAndTalk: Story = {
	parameters: {
		paper: cameraControlsDemo.metadata.paper,
		demo: cameraControlsDemo.metadata.demo
	}
};
