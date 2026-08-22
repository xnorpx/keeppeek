import type { StoryScenarioMetadata } from '../src/lib/storybook/demo';

export const nineCameraLiveStory = {
	storyId: 'demos-live-camera-fleet--nine-random-starts',
	paper: {
		fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
		tokenHash: 'cf3b1cd7',
		boardId: 'MX-0',
		frameId: 'N7-0',
		scenarioId: 'peek.desktop.live-wall'
	},
	demo: {
		title: 'Nine live cameras, nine moments',
		purpose:
			'Prove nine independent, real-time H.264 camera connections against the production KeepPeek WebRTC live wall.',
		narration: {
			voice: 'coral',
			instructions:
				'Speak in a calm, conversational product-demo voice at a natural, unhurried pace. Use subtle emphasis, leave a short breath after each sentence, and do not rush technical terms.',
			cues: [
				{
					atMs: 0,
					text: 'Nine independent virtual cameras are connected to one local KeepPeek server.',
					pauseAfterMs: 450
				},
				{
					atMs: 4_000,
					text: 'Each camera begins at a different randomized moment in the same ten minute film.',
					pauseAfterMs: 500
				},
				{
					atMs: 9_000,
					text: 'KeepPeek carries all nine feeds through the real RTSP and WebRTC path as one live wall.',
					pauseAfterMs: 650
				}
			]
		},
		durationMs: 14_000,
		viewport: { width: 1440, height: 900 },
		captions: [
			{ atMs: 0, text: 'Nine independent virtual cameras are live.' },
			{ atMs: 4_000, text: 'Every camera starts at a different randomized source position.' },
			{ atMs: 9_000, text: 'All nine feeds use the production RTSP and WebRTC path.' }
		],
		actions: [
			{
				kind: 'click',
				atMs: 8_000,
				selector: '[data-camera-id="192.0.2.101"] button[aria-label="WebRTC stream diagnostics"]'
			},
			{
				kind: 'click',
				atMs: 11_500,
				selector: '[data-camera-id="192.0.2.101"] button[aria-label="WebRTC stream diagnostics"]'
			}
		],
		completionSignal: {
			selector: '[data-peek-camera="192.0.2.109"]',
			state: 'visible'
		}
	}
} as const satisfies StoryScenarioMetadata;
