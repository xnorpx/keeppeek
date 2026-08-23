import { describe, expect, it } from 'vitest';
import {
	createDemoWebVtt,
	demoAssetId,
	type StoryScenarioMetadata,
	validateStoryScenarioMetadata
} from './demo';

const validScenario: StoryScenarioMetadata = {
	storyId: 'peek.history-keep',
	paper: {
		fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
		tokenHash: 'cf3b1cd7',
		boardId: '5GD-0',
		frameId: 'history-demo',
		scenarioId: 'peek.desktop.history-keep'
	},
	demo: {
		title: 'Review what just happened',
		purpose: 'Show focused History opening Keep without disturbing other cameras.',
		narration: {
			voice: 'coral',
			instructions: 'Speak clearly in a calm product-demo tone.',
			cues: [
				{ atMs: 0, text: 'First, choose one live camera.' },
				{ atMs: 2_500, text: 'Then open Keep history for that camera.' }
			]
		},
		durationMs: 9_000,
		viewport: { width: 1_440, height: 860 },
		actions: [
			{
				kind: 'click',
				atMs: 2_500,
				selector: '[data-peek-history]'
			}
		],
		completionSignal: { selector: '[data-demo-landed-in-keep]', state: 'visible' },
		captions: [
			{ atMs: 0, text: 'Every camera remains live.' },
			{ atMs: 2_500, text: 'History opens Keep for one focused camera.' },
			{ atMs: 6_500, text: 'Navigate recorded time in Keep.' }
		]
	}
};

describe('Storybook demo metadata', () => {
	it('accepts a complete Paper-linked demo scenario', () => {
		expect(validateStoryScenarioMetadata(validScenario)).toEqual([]);
		expect(
			validateStoryScenarioMetadata({
				...validScenario,
				storyId: 'demos-peek-history--open-keep-history'
			})
		).toEqual([]);
		expect(
			demoAssetId({
				...validScenario,
				demo: { ...validScenario.demo!, assetId: 'cameras.desktop.catalog-guided-setup' }
			})
		).toBe('cameras.desktop.catalog-guided-setup');
		expect(demoAssetId(validScenario)).toBe('peek.desktop.history-keep');
	});

	it('rejects invalid timing and missing story text', () => {
		const issues = validateStoryScenarioMetadata({
			...validScenario,
			storyId: 'Invalid Story',
			demo: {
				...validScenario.demo!,
				assetId: 'Not a valid asset ID',
				title: ' ',
				captions: [
					{ atMs: 4_000, text: 'Later' },
					{ atMs: 3_000, endMs: 10_000, text: '' }
				]
			}
		});

		expect(issues).toEqual(
			expect.arrayContaining([
				{ path: 'storyId', message: 'must be a stable lowercase identifier' },
				{ path: 'demo.assetId', message: 'must be a stable lowercase identifier' },
				{ path: 'demo.title', message: 'must not be empty' },
				{ path: 'demo.captions[1].atMs', message: 'must be later than the previous caption' },
				{ path: 'demo.captions[1].endMs', message: 'must end after the caption starts' },
				{ path: 'demo.captions[1].text', message: 'must not be empty' }
			])
		);
	});

	it('rejects narration settings that cannot fit the demo contract', () => {
		const issues = validateStoryScenarioMetadata({
			...validScenario,
			demo: {
				...validScenario.demo!,
				narration: {
					voice: '',
					instructions: '',
					speed: 4.1,
					cues: [
						{ atMs: 500, text: '' },
						{ atMs: 500, text: 'Duplicate', pauseAfterMs: -1 }
					]
				}
			}
		});

		expect(issues).toEqual(
			expect.arrayContaining([
				{ path: 'demo.narration.voice', message: 'must not be empty' },
				{ path: 'demo.narration.instructions', message: 'must not be empty' },
				{ path: 'demo.narration.speed', message: 'must be between 0.25 and 4' },
				{ path: 'demo.narration.cues[0].atMs', message: 'must start at source time zero' },
				{ path: 'demo.narration.cues[0].text', message: 'must not be empty' },
				{
					path: 'demo.narration.cues[1].atMs',
					message: 'must be later than the previous cue'
				},
				{
					path: 'demo.narration.cues[1].pauseAfterMs',
					message: 'must be a non-negative integer'
				}
			])
		);
	});

	it('generates WebVTT captions using the next caption as the default end', () => {
		expect(createDemoWebVtt(validScenario.demo!)).toBe(`WEBVTT

1
00:00:00.000 --> 00:00:02.500
Every camera remains live.

2
00:00:02.500 --> 00:00:06.500
History opens Keep for one focused camera.

3
00:00:06.500 --> 00:00:09.000
Navigate recorded time in Keep.
`);
	});
});
