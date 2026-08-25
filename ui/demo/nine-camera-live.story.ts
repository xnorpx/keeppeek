import type { StoryScenarioMetadata } from '../src/lib/storybook/demo';

export const nineCameraLiveStory = {
	storyId: 'demos-camera-fleet--add-nine-manual-streams',
	paper: {
		fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
		tokenHash: 'cf3b1cd7',
		boardId: 'MX-0',
		frameId: 'N7-0',
		scenarioId: 'peek.desktop.live-wall'
	},
	demo: {
		title: 'Nine-camera live wall',
		purpose:
			'Prove nine independently paced RTSP cameras render together through the production WebRTC wall.',
		narration: {
			voice: 'coral',
			instructions:
				'Speak in a calm, conversational product-demo voice at a natural, unhurried pace. Use subtle emphasis, leave a short breath after each sentence, and do not rush technical terms.',
			cues: [
				{
					atMs: 0,
					text: 'The Cameras fleet confirms nine independently paced RTSP sources are configured and online.',
					pauseAfterMs: 450
				},
				{
					atMs: 5_000,
					text: 'Open Peek and all nine feeds join the shared WebRTC session as one coordinated live wall.',
					pauseAfterMs: 500
				},
				{
					atMs: 20_000,
					text: 'Each tile advances with real decoded H.264 frames from its own RTSP source.',
					pauseAfterMs: 550
				},
				{
					atMs: 30_000,
					text: 'The stream diagnostics confirm that the wall is using the real RTSP and WebRTC path.',
					pauseAfterMs: 650
				}
			]
		},
		durationMs: 45_000,
		viewport: { width: 1440, height: 900 },
		captions: [
			{ atMs: 0, text: 'The Cameras fleet reports nine configured sources.' },
			{ atMs: 5_000, text: 'The nine feeds join one shared WebRTC live wall.' },
			{ atMs: 20_000, text: 'Every tile advances with decoded H.264 frames.' },
			{ atMs: 30_000, text: 'Diagnostics confirm the production RTSP and WebRTC path.' }
		],
		actions: [
			{ kind: 'click', atMs: 5_000, selector: 'a[aria-label="Peek"]' },
			{
				kind: 'click',
				atMs: 30_000,
				selector: '[data-camera-id="192.0.2.101"] button[data-peek-camera-label]'
			},
			{
				kind: 'click',
				atMs: 38_000,
				selector: '[data-camera-id="192.0.2.101"] button[data-peek-camera-label]'
			}
		],
		completionSignal: {
			selector: '[data-peek-camera="192.0.2.109"]',
			state: 'visible'
		}
	}
} as const satisfies StoryScenarioMetadata;
