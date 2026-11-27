import type { Meta, StoryObj } from '@storybook/svelte';
import { accessControlDemo } from '../demo-scenarios-onboarding';
import DemoAccessStory from './DemoAccessStory.svelte';

const meta = {
	title: 'Demos/Access',
	component: DemoAccessStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records credential creation, per-user camera and group grant scoping, and active session revocation through the production Access runtime and User access dialog.'
			}
		}
	}
} satisfies Meta<typeof DemoAccessStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const CredentialsAndGrants: Story = {
	parameters: {
		paper: accessControlDemo.metadata.paper,
		demo: accessControlDemo.metadata.demo
	}
};
