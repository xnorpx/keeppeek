import type { Meta, StoryObj } from '@storybook/svelte';
import Board18NotificationsStory from './Board18NotificationsStory.svelte';

const meta = {
	title: 'Settings/Notifications',
	component: Board18NotificationsStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 18 notification channels, rule anatomy, quiet-hours policy, and delivery preview rendered from the production evidence owner.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '2YM-0',
			frameId: '2YW-0',
			scenarioId: 'settings.desktop.notifications',
			reference: 'references/18-notifications.png',
			referenceSha256: 'c9c08bab308e95ae0964b5356547adca2e3a5bebb2a42b51348089909a614eee',
			exceptions: [
				'No notification channel registry, configuration, health, browser-permission, or delivery-history contract exists.',
				'Rules, firing counts, cooldowns, quiet hours, retry state, tests, and human delivery actions remain unavailable behind keeppeek.rules.v1.',
				'MQTT and webhook architecture remains a separate integration contract and is not presented as notification delivery evidence.'
			]
		}
	}
} satisfies Meta<typeof Board18NotificationsStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const UnavailableRuntime: Story = {
	name: 'Unavailable runtime'
};
