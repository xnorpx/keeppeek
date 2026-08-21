import type { Meta, StoryObj } from '@storybook/svelte';
import Board04KeepTimelineStory from './Board04KeepTimelineStory.svelte';

const meta = {
	title: 'Keep/Timeline Anatomy',
	component: Board04KeepTimelineStory,
	parameters: {
		viewport: { defaultViewport: 'reset' },
		docs: {
			description: {
				component:
					'Board 4 newest-at-top right-edge timeline rendered through the production availability, gap, activity, event clustering, live-follow, zoom, and playhead owner.'
			}
		},
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: 'C4-0',
			frameId: 'CE-0',
			scenarioId: 'keep.desktop.timeline-anatomy',
			reference: 'references/04-keep-timeline-anatomy.png',
			referenceSha256: '657cafc17086d3b62d803bb78321b791991d1ecb080341dc03fdad72b470176e',
			exceptions: [
				'Event records expose kind and source category rather than Paper’s narrative labels, publisher names, revisions, or multi-frame story counts.',
				'The deterministic player and thumbnail surfaces are neutral; real stored-media playback remains separately covered over WebRTC with native video.',
				'Paper’s hand-positioned ruler uses roughly 207px/hour despite its explanatory 6h row specifying 112px/hour; the override is isolated to this authored raster.'
			]
		}
	}
} satisfies Meta<typeof Board04KeepTimelineStory>;

export default meta;
type Story = StoryObj<typeof meta>;

export const DenseDay: Story = {};
