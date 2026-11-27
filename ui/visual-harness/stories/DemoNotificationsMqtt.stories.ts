import type { Meta, StoryObj } from '@storybook/svelte';
import { notificationsMqttDemo } from '../demo-scenarios-integrations';
import DemoNotificationsMqttStory from './DemoNotificationsMqttStory.svelte';

const meta = {
	title: 'Demos/Notifications MQTT',
	component: DemoNotificationsMqttStory,
	parameters: {
		layout: 'fullscreen',
		docs: {
			description: {
				component:
					'Playwright records notification rules, Inbox/History suppression outcomes, bounded Keeppeek Service mobile push signaling, and MQTT 5 broker health and testing.'
			}
		}
	}
} satisfies Meta<typeof DemoNotificationsMqttStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const RulesPushAndMqtt: Story = {
	parameters: {
		paper: notificationsMqttDemo.metadata.paper,
		demo: notificationsMqttDemo.metadata.demo
	}
};
