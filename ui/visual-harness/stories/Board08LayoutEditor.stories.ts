import type { Meta, StoryObj } from '@storybook/svelte';
import Board08LayoutEditorStory from './Board08LayoutEditorStory.svelte';

const meta = {
	title: 'Peek/Layout Editor',
	component: Board08LayoutEditorStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 8 fixed 12-column editor rendered through the production drag, resize, preset, pin, Activity Focus, undo, and discard owner.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '15P-0',
			frameId: '15Z-0',
			scenarioId: 'peek.desktop.layout-editor',
			reference: 'references/08-layout-editor.png',
			referenceSha256: 'a4c4f51fd31b464751656cce2301b7056bc2cf775348fdfed615ec0e3bf3ee1b',
			exceptions: [
				'No server layout persistence contract exists, so Done remains disabled and the editable draft stays browser-local.',
				'No server layout registry supplies Paper’s 127-source placed count; the story shows three placed of six fixture cameras.',
				'Neutral media surfaces remain candidates until deterministic Peek imagery is approved from Paper.'
			]
		}
	}
} satisfies Meta<typeof Board08LayoutEditorStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Editing: Story = {};
