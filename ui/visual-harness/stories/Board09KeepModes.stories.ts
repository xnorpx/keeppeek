import type { Meta, StoryObj } from '@storybook/svelte';
import Board09KeepModesStory from './Board09KeepModesStory.svelte';

const meta = {
	title: 'Keep/Modes',
	component: Board09KeepModesStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 9 Stories, footage Calendar, gated Export, and shared-clock Swimlanes rendered through the same production owners used by the Keep route.'
			}
		}
	}
} satisfies Meta<typeof Board09KeepModesStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Stories: Story = {
	args: { state: 'stories' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '1BV-0',
			frameId: '1C6-0',
			scenarioId: 'keep.desktop.stories',
			reference: 'references/09-keep-stories.png',
			referenceSha256: 'c453419c776d2094df765fa95bf057eab5e9982a42a06a258c80e94eed9162d2',
			exceptions: [
				'The current event record carries a story kind but no narrative summary or publisher identity.',
				'Only one optional thumbnail URL is available, not a multi-frame attachment sequence.',
				'Source attribution remains the returned camera or KeepPeek category rather than an invented service name.'
			]
		}
	}
};

export const Calendar: Story = {
	args: { state: 'calendar' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '1BV-0',
			frameId: '1DV-0',
			scenarioId: 'keep.desktop.calendar',
			reference: 'references/09-keep-calendar.png',
			referenceSha256: '8f1fe9dae5540b8a8a076d102d6df1199c3e3e90415bf68ad5d4c284a06573ba',
			exceptions: [
				'Timeline availability proves whether footage exists but does not report why a date is unavailable.',
				'The unavailable-day callout therefore names missing footage and explicitly withholds a retention cause.'
			]
		}
	}
};

export const ExportGated: Story = {
	args: { state: 'export' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '1BV-0',
			frameId: '1CX-0',
			scenarioId: 'keep.desktop.export-gated',
			reference: 'references/09-keep-export-gated.png',
			referenceSha256: '5b2278798201fc3d1ff9e8b8b0831ba428dc737563626cc300c65822f2b23a2d',
			exceptions: [
				'Exact keyframe positions are not returned, so the selected segment bounds are editable but not claimed to be snapped.',
				'The fail-closed action names the canonical keeppeek.media-export.v1 capability ID.'
			]
		}
	}
};

export const Swimlanes: Story = {
	args: { state: 'swimlanes' },
	parameters: {
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '1BV-0',
			frameId: '1FB-0',
			scenarioId: 'keep.desktop.swimlanes',
			reference: 'references/09-keep-swimlanes.png',
			referenceSha256: 'cebae9aff8ce25ffac19423d595206f2fe06bd6ee49796774354820d69a3bb15',
			exceptions: [
				'Indexed ranges and event spans are shown without inferring that events across cameras describe the same subject.',
				'Gap treatment reflects missing indexed footage without assigning an outage cause.'
			]
		}
	}
};
