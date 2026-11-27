import type { Meta, StoryObj } from '@storybook/svelte';
import { recordingMaintenanceDemo } from '../demo-scenarios-ops';
import DemoRecordingMaintenanceStory from './DemoRecordingMaintenanceStory.svelte';

const meta = {
	title: 'Demos/Recording Maintenance',
	component: DemoRecordingMaintenanceStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records whole-recording deletion preview, explicit DELETE 2 confirmation, durable Queued to Staged to Deleted progression, and bounded catalog reconciliation with playback-index rebuild.'
			}
		}
	}
} satisfies Meta<typeof DemoRecordingMaintenanceStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const DeleteAndReconcile: Story = {
	parameters: {
		paper: recordingMaintenanceDemo.metadata.paper,
		demo: recordingMaintenanceDemo.metadata.demo
	}
};
