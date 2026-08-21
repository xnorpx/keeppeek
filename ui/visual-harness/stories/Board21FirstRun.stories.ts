import type { Meta, StoryObj } from '@storybook/svelte';
import Board21FirstRunStory from './Board21FirstRunStory.svelte';

const meta = {
	title: 'Setup/First Run and Empty States',
	component: Board21FirstRunStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 21 first-run and empty states rendered from the production setup owners with deterministic observed configuration and health evidence.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '3CX-0',
			frameId: '3D7-0',
			scenarioId: 'setup.desktop.first-run',
			reference: 'references/21-first-run-empty-states.png',
			referenceSha256: '4df4c58bba6272ad07cc6cefc6646377ee9d94046ad06eb1835febadf6b1a650',
			exceptions: [
				'The server reports disk capacity but does not expose a candidate write probe or setup-completion command.',
				'The browser timezone is labeled as browser evidence because the server does not report its timezone.',
				'Identity fields remain behind keeppeek.identity.v1 and are not synthesized.',
				'The event-source registry is unavailable, so the story does not claim that zero sources exist.'
			]
		}
	}
} satisfies Meta<typeof Board21FirstRunStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const NewInstallation: Story = {
	name: 'New installation'
};
