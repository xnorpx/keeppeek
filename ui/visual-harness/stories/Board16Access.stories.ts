import type { Meta, StoryObj } from '@storybook/svelte';
import Board16AccessStory from './Board16AccessStory.svelte';

const meta = {
	title: 'Settings/Access and Roles',
	component: Board16AccessStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 16 target roles, people, tokens, and local/remote access boundary rendered from the production evidence owner.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '2O3-0',
			frameId: '2OD-0',
			scenarioId: 'settings.desktop.access',
			reference: 'references/16-access-roles.png',
			referenceSha256: 'bafb5e6d7e80e269863285084a303d817ace40ca05981d4b00338f25504cdacd',
			exceptions: [
				'The Administrator/User matrix is target policy; the current identity runtime does not enforce or report it.',
				'People, assigned roles, invitations, sessions, current identity, and last-seen evidence are unavailable.',
				'Token listing, creation, rotation, revocation, scopes, owners, last use, and audit records are unavailable; raw key material is never rendered.'
			]
		}
	}
} satisfies Meta<typeof Board16AccessStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const TargetPolicy: Story = {
	name: 'Target policy'
};
