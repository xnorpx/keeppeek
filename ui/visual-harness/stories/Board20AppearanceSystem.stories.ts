import type { Meta, StoryObj } from '@storybook/svelte';
import Board20AppearanceSystemStory from './Board20AppearanceSystemStory.svelte';

const meta = {
	title: 'Settings/Appearance System and Logs',
	component: Board20AppearanceSystemStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 20 Appearance, System, and Logs panels rendered from the production settings owner with deterministic health and redacted-log fixtures.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '39R-0',
			frameId: '3A1-0',
			scenarioId: 'settings.desktop.appearance-system-logs',
			reference: 'references/20-appearance-system-logs.png',
			referenceSha256: '0c672c70765094c6b4e02fdba147e3dd46eab18424774028908606104e76a467',
			exceptions: [
				'The server does not expose timezone, clock, week-start, update-channel, or config-path preferences.',
				'Update check, config backup, recording erase, and full diagnostics-bundle commands remain unavailable.',
				'The implemented redacted log viewer and health route replace Paper actions that imply a broader diagnostics bundle.'
			]
		}
	}
} satisfies Meta<typeof Board20AppearanceSystemStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Evidence: Story = {
	name: 'Observed system evidence'
};
