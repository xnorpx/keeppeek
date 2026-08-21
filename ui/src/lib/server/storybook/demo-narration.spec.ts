import { describe, expect, it } from 'vitest';
import type { StoryScenarioMetadata } from '$lib/storybook/demo';
import { createNarratedDemoPlan } from './demo-video';
import {
	assertDemoNarrationManifest,
	createNarratedDemoWebVtt,
	type DemoNarrationManifest
} from './demo-narration';

const metadata: StoryScenarioMetadata = {
	storyId: 'demo-paced--first-then',
	paper: {
		fileId: 'paper',
		tokenHash: '00000000',
		boardId: 'board',
		frameId: 'frame',
		scenarioId: 'demo.paced'
	},
	demo: {
		title: 'Paced demo',
		purpose: 'Verify audio-led pacing.',
		durationMs: 5_000,
		viewport: { width: 320, height: 180 },
		narration: {
			voice: 'coral',
			instructions: 'Speak clearly.',
			cues: [
				{ atMs: 0, text: 'First, do this.', pauseAfterMs: 400 },
				{ atMs: 2_000, text: 'Then, do that.' }
			]
		},
		captions: [{ atMs: 0, text: 'Demo' }],
		actions: [{ kind: 'click', atMs: 1_000, selector: 'button' }],
		completionSignal: { selector: 'main', state: 'visible' }
	}
};

const manifest: DemoNarrationManifest = {
	schemaVersion: 1,
	storyId: metadata.storyId,
	scenarioId: metadata.paper.scenarioId,
	commitSha: 'abc123',
	generatedAt: '2026-08-21T00:00:00.000Z',
	deployment: 'keeppeek-demo-tts',
	voice: 'coral',
	instructions: 'Speak clearly.',
	cues: [
		{
			index: 0,
			sourceAtMs: 0,
			pauseAfterMs: 400,
			text: 'First, do this.',
			fileName: '001.wav',
			durationMs: 2_600,
			bytes: 100,
			sha256: 'a'.repeat(64)
		},
		{
			index: 1,
			sourceAtMs: 2_000,
			pauseAfterMs: 0,
			text: 'Then, do that.',
			fileName: '002.wav',
			durationMs: 1_500,
			bytes: 100,
			sha256: 'b'.repeat(64)
		}
	]
};

describe('paced demo narration artifacts', () => {
	it('validates measured WAVs against immutable story cues', () => {
		expect(() => assertDemoNarrationManifest(metadata, manifest)).not.toThrow();
		expect(() =>
			assertDemoNarrationManifest(metadata, {
				...manifest,
				cues: [{ ...manifest.cues[0]!, text: 'Changed' }, manifest.cues[1]!]
			})
		).toThrow('does not match story metadata');
	});

	it('writes spoken captions on the expanded output timeline', () => {
		const plan = createNarratedDemoPlan(5_000, [
			{ sourceAtMs: 0, audioPath: '001.wav', audioDurationMs: 2_600, pauseAfterMs: 400 },
			{ sourceAtMs: 2_000, audioPath: '002.wav', audioDurationMs: 1_500 }
		]);
		expect(createNarratedDemoWebVtt(metadata.demo!.narration!, plan)).toBe(`WEBVTT

1
00:00:00.000 --> 00:00:02.600
First, do this.

2
00:00:03.000 --> 00:00:04.500
Then, do that.
`);
	});
});
