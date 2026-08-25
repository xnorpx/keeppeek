import type { Meta, StoryObj } from '@storybook/svelte';
import Board27MobileAdministrationStory from './Board27MobileAdministrationStory.svelte';

const meta = {
	title: 'Settings/Mobile Administration',
	component: Board27MobileAdministrationStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 27 mobile administration index rendered with the production settings index, header, and primary navigation using deterministic WebRTC evidence.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '40J-0',
			frameId: '4I6-0',
			scenarioId: 'settings.mobile.administration',
			reference: 'references/27-mobile-administration-index.png',
			referenceSha256: '1e656b06d3599f159daf029cd1fd40faf258cafdb52341affc6acb7a6da4133d',
			exceptions: [
				'Unavailable event-source, group, rule, and identity counts render as em dashes rather than fixture-only numbers.',
				'The header identifies the verified local connection rather than fabricating a signed-in person.'
			]
		}
	}
} satisfies Meta<typeof Board27MobileAdministrationStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Index: Story = {
	name: 'Administration index',
	args: { state: 'index' }
};

export const Access: Story = {
	name: 'Access',
	args: { state: 'access' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '40J-0',
			frameId: '4M9-0',
			scenarioId: 'settings.mobile.access',
			reference: 'references/27-mobile-access.png',
			referenceSha256: '2055c42ff058345ad88b6a1c7f9f24da4f1a77a6b8fbd1952832860943bd2e39'
		}
	}
};
