import type { StoryScenarioMetadata } from '../src/lib/storybook/demo';

const cameraFormActions = Array.from({ length: 9 }, (_, index) => {
	const addAtMs = 4_000 + index * 3_500;
	return [
		{ kind: 'click' as const, atMs: addAtMs, selector: 'role=button[name="Add camera"]' },
		{ kind: 'click' as const, atMs: addAtMs + 2_300, selector: 'role=button[name="Save camera"]' }
	];
}).flat();

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
		title: 'Add nine cameras in Settings',
		purpose:
			'Prove nine manual RTSP camera configurations can be entered through production Settings and become live immediately.',
		narration: {
			voice: 'coral',
			instructions:
				'Speak in a calm, conversational product-demo voice at a natural, unhurried pace. Use subtle emphasis, leave a short breath after each sentence, and do not rush technical terms.',
			cues: [
				{
					atMs: 0,
					text: 'We begin with an empty KeepPeek server and nine virtual RTSP cameras ready to add.',
					pauseAfterMs: 450
				},
				{
					atMs: 2_000,
					text: 'Open Settings, then add each camera with its address, credentials, and main and sub stream URLs.',
					pauseAfterMs: 500
				},
				{
					atMs: 16_000,
					text: 'Every form is saved through the production control channel. Each camera starts as soon as its configuration is accepted.',
					pauseAfterMs: 550
				},
				{
					atMs: 34_000,
					text: 'The ninth camera completes the fleet. Every configuration was entered here in Settings.',
					pauseAfterMs: 550
				},
				{
					atMs: 38_000,
					text: 'Restart the recorder once, so KeepPeek loads the complete nine-camera configuration we just saved.',
					pauseAfterMs: 600
				},
				{
					atMs: 48_000,
					text: 'Return to Peek and all nine independently paced feeds appear together on the live wall.',
					pauseAfterMs: 550
				},
				{
					atMs: 60_000,
					text: 'The stream diagnostics confirm that the wall is using the real RTSP and WebRTC path.',
					pauseAfterMs: 650
				}
			]
		},
		durationMs: 70_000,
		viewport: { width: 1440, height: 900 },
		captions: [
			{ atMs: 0, text: 'Begin with an empty KeepPeek server.' },
			{ atMs: 2_000, text: 'Add nine RTSP cameras manually in Settings.' },
			{ atMs: 16_000, text: 'Each saved camera starts through the production control path.' },
			{ atMs: 34_000, text: 'All nine camera configurations are saved.' },
			{ atMs: 38_000, text: 'Restart once with the complete saved configuration.' },
			{ atMs: 48_000, text: 'The nine live feeds appear together in Peek.' },
			{ atMs: 60_000, text: 'Diagnostics confirm the production RTSP and WebRTC path.' }
		],
		actions: [
			{ kind: 'click', atMs: 2_000, selector: 'a[aria-label="Settings"]' },
			...cameraFormActions,
			{ kind: 'click', atMs: 38_000, selector: 'role=button[name="Restart"]' },
			{ kind: 'click', atMs: 48_000, selector: 'a[aria-label="Peek"]' },
			{
				kind: 'click',
				atMs: 60_000,
				selector: '[data-camera-id="192.0.2.101"] button[data-peek-camera-label]'
			},
			{
				kind: 'click',
				atMs: 65_000,
				selector: '[data-camera-id="192.0.2.101"] button[data-peek-camera-label]'
			}
		],
		completionSignal: {
			selector: '[data-peek-camera="192.0.2.109"]',
			state: 'visible'
		}
	}
} as const satisfies StoryScenarioMetadata;
