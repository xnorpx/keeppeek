import type { Meta, StoryObj } from '@storybook/svelte';
import { liveWallEditorDemo } from '../demo-scenarios-onboarding';
import DemoLiveWallEditorStory from './DemoLiveWallEditorStory.svelte';

const meta = {
	title: 'Demos/Live Wall',
	component: DemoLiveWallEditorStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records 12-column dashboard layout editing, per-dashboard Wall display settings, and dashboard audience scoping.'
			}
		}
	}
} satisfies Meta<typeof DemoLiveWallEditorStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const LayoutEditor: Story = {
	parameters: {
		paper: liveWallEditorDemo.metadata.paper,
		demo: liveWallEditorDemo.metadata.demo
	}
};
