import type { DemoScenarioDefinition } from '../src/lib/storybook/demo';

export const board31HistoryDemo = {
	metadata: {
		storyId: 'demos-peek-history--open-keep-history',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '5GD-0',
			frameId: '5I0-0',
			scenarioId: 'peek.desktop.history-keep'
		},
		demo: {
			title: 'Review what just happened',
			purpose: 'Show a focused camera opening Keep for deliberate timeline review.',
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
						text: 'Next, use History to open Keep for that focused camera.',
						pauseAfterMs: 300
					},
					{
						atMs: 6_000,
						text: 'Keep opens at the live edge, ready for deliberate timeline review.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 9_000,
			viewport: { width: 928, height: 524 },
			captions: [
				{ atMs: 0, text: 'Focus keeps the overview clean.' },
				{ atMs: 2_000, text: 'History opens Keep for the focused camera.' },
				{ atMs: 6_000, text: 'Keep owns timeline navigation and review.' }
			],
			actions: [
				{
					kind: 'click',
					atMs: 2_000,
					selector: '[data-peek-history]'
				}
			],
			completionSignal: { selector: '[data-demo-landed-in-keep]', state: 'visible' }
		}
	},
	previewScenarioId: 'peek.desktop.history-keep',
	storySource: 'visual-harness/stories/DemoHistory.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoHistoryStory.svelte',
		'visual-harness/stories/Board31HistoryStory.svelte',
		'src/routes/+page.svelte'
	]
} as const satisfies DemoScenarioDefinition;

export const cameraCatalogWizardDemo = {
	metadata: {
		storyId: 'demos-camera-catalog-wizard--guided-setup',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '1VG-0',
			frameId: '1VQ-0',
			scenarioId: 'cameras.desktop.add-wizard'
		},
		demo: {
			assetId: 'cameras.desktop.catalog-guided-setup',
			title: 'Set up a camera with catalog context',
			purpose:
				'Show discovery enrichment, optional manual catalog research, editable stream suggestions, and review before the first configuration write.',
			narration: {
				voice: 'coral',
				instructions:
					'Use a confident, concise product-demo voice without overselling unverified evidence.',
				cues: [
					{
						atMs: 0,
						text: 'Adding a camera begins with context, not a blank form.',
						pauseAfterMs: 250
					},
					{
						atMs: 1_800,
						text: 'Discovery identifies the device. When its model matches, KeepPeek adds the catalog facts it knows.',
						pauseAfterMs: 250
					},
					{
						atMs: 4_500,
						text: 'Need deeper manual research? Open CCTV Database in a separate tab while the draft stays here.',
						pauseAfterMs: 250
					},
					{
						atMs: 6_500,
						text: 'For a quiet camera, enter its address and search the model directly.',
						pauseAfterMs: 250
					},
					{
						atMs: 12_000,
						text: 'Catalog stream URLs fill automatically when ONVIF has not supplied endpoints. They remain editable suggestions.',
						pauseAfterMs: 300
					},
					{
						atMs: 16_000,
						text: 'Review every value before saving. The final button is the first configuration write.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 21_000,
			viewport: { width: 1280, height: 800 },
			captions: [
				{ atMs: 0, text: 'Add cameras with discovery and catalog context.' },
				{ atMs: 1_800, text: 'Use a catalog match to fill the facts KeepPeek knows.' },
				{ atMs: 4_500, text: 'Open CCTV Database for deeper manual research.' },
				{ atMs: 6_500, text: 'Search a quiet camera by address and model.' },
				{ atMs: 12_000, text: 'Catalog stream suggestions fill automatically when needed.' },
				{ atMs: 16_000, text: 'Review first. Save is the first configuration write.' }
			],
			actions: [
				{ kind: 'click', atMs: 1_800, selector: '[data-demo-action="use-discovery-match"]' },
				{ kind: 'click', atMs: 4_500, selector: '[data-demo-action="open-catalog-source"]' },
				{ kind: 'click', atMs: 6_500, selector: '[data-demo-action="manual-search"]' },
				{ kind: 'click', atMs: 8_000, selector: '[data-demo-action="run-model-search"]' },
				{ kind: 'click', atMs: 10_000, selector: '[data-demo-action="select-catalog-model"]' },
				{ kind: 'click', atMs: 16_000, selector: '[data-demo-action="review-setup"]' }
			],
			completionSignal: { selector: '[data-demo-catalog-reviewed="true"]', state: 'visible' }
		}
	},
	previewScenarioId: 'cameras.desktop.add-wizard',
	storySource: 'visual-harness/stories/DemoCameraCatalogWizard.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoCameraCatalogWizardStory.svelte',
		'visual-harness/stories/DemoCameraCatalogWizard.stories.ts',
		'src/lib/components/CameraCatalogEvidence.svelte',
		'src/lib/components/DesktopCameraWizardStreamsStep.svelte',
		'src/lib/camera-wizard.ts'
	]
} as const satisfies DemoScenarioDefinition;

export const demoScenarios: readonly DemoScenarioDefinition[] = [
	board31HistoryDemo,
	cameraCatalogWizardDemo
];
