import type { StoryScenarioMetadata } from '../src/lib/storybook/demo';

export const cameraLifecycleStory = {
	storyId: 'demos-camera-lifecycle--delete-and-readd',
	paper: {
		fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
		tokenHash: 'cf3b1cd7',
		boardId: '1VG-0',
		frameId: '1VQ-0',
		scenarioId: 'cameras.desktop.add-wizard'
	},
	demo: {
		title: 'Delete and re-add a camera',
		purpose:
			'Prove camera configuration changes against a real KeepPeek server and a deterministic H.264 camera.',
		durationMs: 22_000,
		viewport: { width: 1280, height: 720 },
		captions: [
			{ atMs: 0, text: 'The local H.264 camera is decoded and stable.' },
			{ atMs: 2_000, text: 'Open Settings and locate the configured camera.' },
			{ atMs: 5_000, text: 'Remove the camera configuration.' },
			{ atMs: 8_000, text: 'Re-enter the same camera configuration.' },
			{ atMs: 16_000, text: 'The same camera is configured again and ready to apply.' }
		],
		actions: [
			{ kind: 'click', atMs: 2_000, selector: 'a[aria-label="Settings"]' },
			{ kind: 'click', atMs: 5_000, selector: 'role=button[name="Remove"]' },
			{ kind: 'click', atMs: 8_000, selector: 'role=button[name="Add camera"]' },
			{ kind: 'click', atMs: 16_000, selector: 'role=button[name="Save camera"]' }
		],
		completionSignal: {
			selector: 'section[aria-labelledby="configured-cameras-title"] a[href="/?camera=127.0.0.1"]',
			state: 'visible'
		}
	}
} as const satisfies StoryScenarioMetadata;
