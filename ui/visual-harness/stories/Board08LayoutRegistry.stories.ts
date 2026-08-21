import type { Meta, StoryObj } from '@storybook/svelte';
import Board08LayoutRegistryStory from './Board08LayoutRegistryStory.svelte';

const meta = {
	title: 'Peek/Layout Registry',
	component: Board08LayoutRegistryStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 8 server-layout switcher and deletion targets rendered as honest unavailable states because no registry or CRUD contract exists.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '15P-0',
			frameId: '1AE-0',
			scenarioId: 'peek.desktop.layout-registry',
			reference: 'references/08-layout-registry-dialogs.png',
			referenceSha256: 'c27ed38545b228b1c0a948ab6291a40f6b25b4859e2857eef9530ac083127ec6',
			exceptions: [
				'No server layout directory returns names, camera counts, stable identities, or current selection.',
				'No create, rename, persist, or delete layout command exists in the canonical WebRTC API.',
				'Fixture-only layout names and destructive targets are therefore never rendered.'
			]
		}
	}
} satisfies Meta<typeof Board08LayoutRegistryStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Unavailable: Story = {};
