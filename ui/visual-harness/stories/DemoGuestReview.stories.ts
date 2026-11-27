import type { Meta, StoryObj } from '@storybook/svelte';
import { guestReviewDemo } from '../demo-scenarios-watch';
import DemoGuestReviewStory from './DemoGuestReviewStory.svelte';

const meta = {
	title: 'Demos/Guest Review',
	component: DemoGuestReviewStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records configuring Guest sharing with a canonical HTTPS origin, issuing a one-time link from a ready export, previewing the isolated /share surface, and revoking the grant with a reason.'
			}
		}
	}
} satisfies Meta<typeof DemoGuestReviewStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const PolicyShareRevoke: Story = {
	parameters: {
		paper: guestReviewDemo.metadata.paper,
		demo: guestReviewDemo.metadata.demo
	}
};
