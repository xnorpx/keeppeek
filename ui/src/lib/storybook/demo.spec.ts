import { describe, expect, it } from 'vitest';
import {
	createDemoWebVtt,
	type StoryScenarioMetadata,
	validateStoryScenarioMetadata
} from './demo';

const validScenario: StoryScenarioMetadata = {
	storyId: 'peek.rewind-to-keep',
	paper: {
		fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
		tokenHash: 'cf3b1cd7',
		boardId: '5GD-0',
		frameId: 'rewind-demo',
		scenarioId: 'peek.desktop.rewind-to-keep'
	},
	demo: {
		title: 'Review what just happened',
		purpose: 'Show the live-to-recorded transition without disturbing other cameras.',
		narration: {
			text: 'Drag backward on one camera to review the last two minutes.',
			voice: 'coral',
			instructions: 'Speak clearly in a calm product-demo tone.',
			startAtMs: 500
		},
		durationMs: 9_000,
		viewport: { width: 1_440, height: 860 },
		actions: [
			{
				kind: 'pointer-drag',
				atMs: 2_500,
				selector: '[aria-label="Rewind Front Door"]',
				deltaX: 0,
				deltaY: 160,
				durationMs: 1_000,
				holdAfterMs: 2_000
			}
		],
		completionSignal: { selector: '[data-peek-rewind]', state: 'hidden' },
		captions: [
			{ atMs: 0, text: 'Every camera remains live.' },
			{ atMs: 2_500, text: 'Drag one tile back 38 seconds.' },
			{ atMs: 6_500, text: 'Continue at that moment in Keep.' }
		]
	}
};

describe('Storybook demo metadata', () => {
	it('accepts a complete Paper-linked demo scenario', () => {
		expect(validateStoryScenarioMetadata(validScenario)).toEqual([]);
		expect(
			validateStoryScenarioMetadata({
				...validScenario,
				storyId: 'demos-peek-rewind--rewind-one-camera'
			})
		).toEqual([]);
	});

	it('rejects invalid timing and missing story text', () => {
		const issues = validateStoryScenarioMetadata({
			...validScenario,
			storyId: 'Invalid Story',
			demo: {
				...validScenario.demo!,
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
					text: '',
					voice: '',
					instructions: '',
					startAtMs: 9_000,
					speed: 4.1
				}
			}
		});

		expect(issues).toEqual(
			expect.arrayContaining([
				{ path: 'demo.narration.text', message: 'must not be empty' },
				{ path: 'demo.narration.voice', message: 'must not be empty' },
				{ path: 'demo.narration.instructions', message: 'must not be empty' },
				{ path: 'demo.narration.startAtMs', message: 'must occur within the demo duration' },
				{ path: 'demo.narration.speed', message: 'must be between 0.25 and 4' }
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
Drag one tile back 38 seconds.

3
00:00:06.500 --> 00:00:09.000
Continue at that moment in Keep.
`);
	});
});
