import type { Meta, StoryObj } from '@storybook/svelte';
import { fleetConfigDemo } from '../demo-scenarios-ops';
import DemoFleetConfigStory from './DemoFleetConfigStory.svelte';

const meta = {
	title: 'Demos/Fleet Config',
	component: DemoFleetConfigStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records camera fleet selection, Overview provenance inspection, secret-referencing templates, server-owned bulk diff preview, and per-camera activation status.'
			}
		}
	}
} satisfies Meta<typeof DemoFleetConfigStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const TemplatesAndBulkApply: Story = {
	parameters: {
		paper: fleetConfigDemo.metadata.paper,
		demo: fleetConfigDemo.metadata.demo
	}
};
