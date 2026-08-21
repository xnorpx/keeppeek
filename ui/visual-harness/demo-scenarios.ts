import type { DemoScenarioDefinition } from '../src/lib/storybook/demo';

export const board31RewindDemo = {
	metadata: {
		storyId: 'demos-peek-rewind--rewind-one-camera',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '5GD-0',
			frameId: '5HF-0',
			scenarioId: 'peek.desktop.rewind-to-keep'
		},
		demo: {
			title: 'Review what just happened',
			purpose: 'Show a camera rewind gesture and the selected moment opening in Keep.',
			narration: {
				voice: 'coral',
				instructions: 'Use a calm, concise product-demo voice with clear transitions.',
				cues: [
					{
						atMs: 0,
						text: 'First, choose the live camera you want to review.',
						pauseAfterMs: 250
					},
					{
						atMs: 2_000,
						text: 'Next, drag down on that camera to rewind without interrupting the live wall.',
						pauseAfterMs: 300
					},
					{
						atMs: 6_000,
						text: 'Then Keep opens at the selected moment, ready for detailed review.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 9_000,
			viewport: { width: 928, height: 524 },
			captions: [
				{ atMs: 0, text: 'The camera remains live and ready.' },
				{ atMs: 2_000, text: 'Drag down to rewind one camera.' },
				{ atMs: 6_000, text: 'Keep opens at the selected moment, ready to review.' }
			],
			actions: [
				{
					kind: 'pointer-drag',
					atMs: 2_000,
					selector: '[aria-label="Rewind Front Door"]',
					deltaX: 0,
					deltaY: 166,
					durationMs: 1_500,
					holdAfterMs: 2_500,
					steps: 30
				}
			],
			completionSignal: { selector: '[data-demo-landed-in-keep]', state: 'visible' }
		}
	},
	previewScenarioId: 'peek.desktop.rewind-to-keep',
	storySource: 'visual-harness/stories/DemoRewind.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoRewindStory.svelte',
		'visual-harness/stories/Board31RewindStory.svelte',
		'src/lib/components/PeekCameraTile.svelte',
		'src/lib/components/PeekEntryBanner.svelte',
		'src/lib/components/PeekRewindControl.svelte',
		'src/lib/components/PeekRewindState.svelte'
	]
} as const satisfies DemoScenarioDefinition;

export const demoScenarios: readonly DemoScenarioDefinition[] = [board31RewindDemo];
