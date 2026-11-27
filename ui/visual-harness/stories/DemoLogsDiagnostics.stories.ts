import type { Meta, StoryObj } from '@storybook/svelte';
import { logsDiagnosticsDemo } from '../demo-scenarios-integrations';
import DemoLogsDiagnosticsStory from './DemoLogsDiagnosticsStory.svelte';

const meta = {
	title: 'Demos/Logs Diagnostics',
	component: DemoLogsDiagnosticsStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records theme switching and runtime details, live log filtering and pausing, scrubbed diagnostics export, and the in-app bug report redaction review.'
			}
		}
	}
} satisfies Meta<typeof DemoLogsDiagnosticsStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const AppearanceLogsAndBugReport: Story = {
	parameters: {
		paper: logsDiagnosticsDemo.metadata.paper,
		demo: logsDiagnosticsDemo.metadata.demo
	}
};
