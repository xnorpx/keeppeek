import type { Meta, StoryObj } from '@storybook/svelte';
import Board19GroupsStory from './Board19GroupsStory.svelte';

const meta = {
	title: 'Settings/Groups and Two-Way Audio',
	component: Board19GroupsStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 19 group administration and participant contracts rendered from the production evidence owner without fixture-only groups or people.'
			}
		}
	}
} satisfies Meta<typeof Board19GroupsStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Administration: Story = {
	args: { state: 'administration' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '35C-0',
			frameId: '35M-0',
			scenarioId: 'groups.desktop.administration',
			reference: 'references/19-groups-administration.png',
			referenceSha256: '1e362cd0e3ac84de2a44ccd7d3d638da33e27eba9db6e8cf1ffbee85337901af',
			exceptions: [
				'ListGroups has generated protobuf types but no server or browser handler, so no group names, definitions, or presence can be shown.',
				'Group definition administration remains gated by keeppeek.group-admin.v1.'
			]
		}
	}
};

export const Participant: Story = {
	args: { state: 'participant' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '35C-0',
			frameId: '380-0',
			scenarioId: 'groups.desktop.participant',
			reference: 'references/19-groups-participant.png',
			referenceSha256: '7a0fcac70292a43501fb7570121d635e2ff557face17b99bd3a227d3b383d5e3',
			exceptions: [
				'JoinGroup and GroupState have no runtime handler, so joined duration, recording state, round-trip time, participant names, and speaking state are unavailable.',
				'Push-to-talk remains a client-side contract and is disabled until a real group join exists.'
			]
		}
	}
};
