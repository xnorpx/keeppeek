import type { StoryScenarioMetadata } from '../src/lib/storybook/demo';

export const cameraLifecycleStory = {
	storyId: 'demos-camera-lifecycle--add-verified-camera',
	paper: {
		fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
		tokenHash: 'cf3b1cd7',
		boardId: '1VG-0',
		frameId: '1VQ-0',
		scenarioId: 'cameras.desktop.add-wizard'
	},
	demo: {
		title: 'Add a verified camera',
		purpose:
			'Prove the add-camera wizard against a real KeepPeek server and a deterministic H.264 camera.',
		narration: {
			voice: 'coral',
			instructions: 'Use a calm, concise product-demo voice with clear transitions.',
			cues: [
				{
					atMs: 0,
					text: 'Start with an empty camera fleet and a local H.264 camera ready to connect.',
					pauseAfterMs: 250
				},
				{
					atMs: 2_000,
					text: 'Open the add-camera wizard and enter the camera address and credentials.',
					pauseAfterMs: 250
				},
				{
					atMs: 7_000,
					text: 'Confirm the protocol, transport, and service ports before continuing.',
					pauseAfterMs: 250
				},
				{
					atMs: 15_000,
					text: 'Verify both RTSP streams. KeepPeek requires video and a keyframe before saving.',
					pauseAfterMs: 300
				},
				{
					atMs: 23_000,
					text: 'Choose the camera name and recording policy, then review the complete draft.',
					pauseAfterMs: 300
				},
				{
					atMs: 31_000,
					text: 'Save the verified camera. KeepPeek starts it immediately and reports the result.',
					pauseAfterMs: 300
				}
			]
		},
		durationMs: 36_000,
		viewport: { width: 1280, height: 720 },
		captions: [
			{ atMs: 0, text: 'The camera fleet starts empty.' },
			{ atMs: 2_000, text: 'Enter the camera address and credentials.' },
			{ atMs: 7_000, text: 'Confirm the connection options.' },
			{ atMs: 15_000, text: 'Verify video and keyframes from both streams.' },
			{ atMs: 23_000, text: 'Name the camera and review its recording policy.' },
			{ atMs: 31_000, text: 'Save and start the verified camera.' }
		],
		actions: [
			{ kind: 'click', atMs: 2_000, selector: 'role=link[name="Add camera"]' },
			{ kind: 'click', atMs: 7_000, selector: 'role=button[name="Continue"]' },
			{ kind: 'click', atMs: 11_000, selector: 'role=button[name="Continue"]' },
			{ kind: 'click', atMs: 15_000, selector: 'role=button[name="Verify streams"]' },
			{ kind: 'click', atMs: 23_000, selector: 'role=button[name="Continue"]' },
			{ kind: 'click', atMs: 27_000, selector: 'role=button[name="Continue"]' },
			{ kind: 'click', atMs: 31_000, selector: 'role=button[name="Save camera"]' }
		],
		completionSignal: {
			selector: 'section[aria-label="Camera saved"]',
			state: 'visible'
		}
	}
} as const satisfies StoryScenarioMetadata;
