import type { DemoScenarioDefinition } from '../src/lib/storybook/demo';

export const firstRunDemo = {
	metadata: {
		storyId: 'demos-first-run--guided-setup',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '3CX-0',
			frameId: '3D7-0',
			scenarioId: 'setup.desktop.first-run'
		},
		demo: {
			assetId: 'setup.desktop.first-run',
			title: 'Complete first-run setup',
			purpose:
				'Verify local recording storage, review browser time zone notice, claim the one-time initial Administrator key, and enter the empty-fleet state.',
			narration: {
				voice: 'coral',
				instructions: 'Use a calm, concise product-demo voice with clear transitions.',
				cues: [
					{
						atMs: 0,
						text: 'First-run setup verifies local storage, browser time, and remote access readiness before recording starts.',
						pauseAfterMs: 250
					},
					{
						atMs: 2_200,
						text: 'Run the storage write probe to confirm the recording path is writable and enable camera setup.',
						pauseAfterMs: 250
					},
					{
						atMs: 5_800,
						text: 'Retrieve the one-time initial Administrator key if you need remote access, and save it immediately.',
						pauseAfterMs: 250
					},
					{
						atMs: 9_200,
						text: 'Hide the key permanently once saved, then continue to camera setup.',
						pauseAfterMs: 250
					},
					{
						atMs: 11_500,
						text: 'KeepPeek opens to an empty camera list, ready to scan your network or add a camera.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 15_000,
			viewport: { width: 1280, height: 800 },
			captions: [
				{
					atMs: 0,
					text: 'Inspect storage capacity, browser time zone, and remote readiness on /setup.'
				},
				{ atMs: 2_200, text: 'Run the storage write probe to verify recording path access.' },
				{
					atMs: 5_800,
					text: 'Retrieve the one-time initial Administrator key for remote sign-in.'
				},
				{ atMs: 9_200, text: 'Hide the initial key permanently after storing it safely.' },
				{ atMs: 11_500, text: 'Continue into the empty-fleet workspace to add your first camera.' }
			],
			actions: [
				{ kind: 'click', atMs: 2_200, selector: 'button:has-text("Verify storage again")' },
				{ kind: 'click', atMs: 5_800, selector: 'button:has-text("Retrieve initial key")' },
				{ kind: 'click', atMs: 9_200, selector: 'button:has-text("Hide permanently")' },
				{ kind: 'click', atMs: 11_500, selector: 'button:has-text("Continue to camera setup")' }
			],
			completionSignal: { selector: '[data-demo-first-run-ready="true"]', state: 'visible' }
		}
	},
	previewScenarioId: 'setup.desktop.first-run',
	storySource: 'visual-harness/stories/DemoFirstRun.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoFirstRunStory.svelte',
		'visual-harness/stories/DemoFirstRun.stories.ts',
		'src/lib/components/FirstRunSetupPanel.svelte',
		'src/lib/components/FirstRunEmptyStates.svelte',
		'src/lib/components/InitialAccessKeyClaim.svelte'
	]
} as const satisfies DemoScenarioDefinition;

export const accessControlDemo = {
	metadata: {
		storyId: 'demos-access--credentials-and-grants',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '2O3-0',
			frameId: '2OD-0',
			scenarioId: 'settings.desktop.access'
		},
		demo: {
			assetId: 'settings.desktop.access',
			title: 'Manage credentials, camera grants, and sessions',
			purpose:
				'Create a named User credential, scope its group and camera grants in User access, and revoke an active remote session.',
			narration: {
				voice: 'coral',
				instructions: 'Use a calm, concise product-demo voice with clear security boundaries.',
				cues: [
					{
						atMs: 0,
						text: 'Access settings separate trusted local administration from named remote credentials and active sessions.',
						pauseAfterMs: 250
					},
					{
						atMs: 1_800,
						text: 'Create a dedicated User credential for each person or screen. Its bearer key is shown only once.',
						pauseAfterMs: 250
					},
					{
						atMs: 8_800,
						text: 'Open User access to narrow the credential from all cameras to selected groups and individual cameras.',
						pauseAfterMs: 250
					},
					{
						atMs: 14_500,
						text: 'Saving camera grants updates server enforcement immediately and records the change in the audit log.',
						pauseAfterMs: 250
					},
					{
						atMs: 17_000,
						text: 'Revoke any active session immediately from the sessions list to close its WebRTC connection.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 20_000,
			viewport: { width: 1280, height: 800 },
			captions: [
				{
					atMs: 0,
					text: 'Review local Administrator trust, named credentials, and active sessions.'
				},
				{ atMs: 1_800, text: 'Create a named User credential with a one-time bearer key.' },
				{ atMs: 8_800, text: 'Open User access to scope groups and individual cameras.' },
				{ atMs: 14_500, text: 'Save camera grants to enforce updated server-side permissions.' },
				{ atMs: 17_000, text: 'Revoke an active session and confirm the updated session list.' }
			],
			actions: [
				{ kind: 'click', atMs: 1_800, selector: '[data-new-access-credential]' },
				{
					kind: 'fill',
					atMs: 2_800,
					selector: 'label:has-text("Name") input',
					value: 'Night Guard'
				},
				{
					kind: 'fill',
					atMs: 3_800,
					selector: 'label:has-text("Description") input',
					value: 'Overnight gate viewer'
				},
				{ kind: 'select', atMs: 4_800, selector: 'label:has-text("Role") select', value: 'user' },
				{ kind: 'click', atMs: 5_800, selector: 'form button[type="submit"]' },
				{
					kind: 'click',
					atMs: 8_800,
					selector: 'button[aria-label="User access for Night Guard"]'
				},
				{
					kind: 'click',
					atMs: 10_500,
					selector: 'dialog[data-user-access-paper] label:has-text("Selected groups and cameras")'
				},
				{
					kind: 'click',
					atMs: 12_000,
					selector: 'dialog[data-user-access-paper] input[type="checkbox"][aria-label="outdoor"]'
				},
				{
					kind: 'click',
					atMs: 13_200,
					selector: 'dialog[data-user-access-paper] input[type="checkbox"][aria-label="Back Door"]'
				},
				{
					kind: 'click',
					atMs: 14_500,
					selector: 'dialog[data-user-access-paper] button:has-text("Save access")'
				},
				{
					kind: 'click',
					atMs: 17_000,
					selector:
						'section[aria-labelledby="sessions-heading"] div.flex:has-text("Front Desk Tablet") button[title="Revoke session"]'
				}
			],
			completionSignal: { selector: '[data-demo-access-complete="true"]', state: 'visible' }
		}
	},
	previewScenarioId: 'settings.desktop.access',
	storySource: 'visual-harness/stories/DemoAccess.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoAccessStory.svelte',
		'visual-harness/stories/DemoAccess.stories.ts',
		'src/lib/components/AccessRuntime.svelte',
		'src/lib/components/CameraAccessDialog.svelte',
		'src/lib/components/DesktopPaperRail.svelte'
	]
} as const satisfies DemoScenarioDefinition;

export const cameraControlsDemo = {
	metadata: {
		storyId: 'demos-camera-controls--ptz-and-talk',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: 'TS-0',
			frameId: 'U2-0',
			scenarioId: 'camera.desktop.details-ptz'
		},
		demo: {
			assetId: 'camera.desktop.details-ptz',
			title: 'Operate PTZ, motion detection, and Talk',
			purpose:
				'Steer continuous PTZ with explicit stop, recall a camera preset, toggle hardware motion detection with read-back, and show Administrator browser Talk.',
			narration: {
				voice: 'coral',
				instructions:
					'Use a calm, concise product-demo voice grounded in verified camera feedback.',
				cues: [
					{
						atMs: 0,
						text: 'The camera detail page brings live preview, stream profiles, and device controls into one workspace.',
						pauseAfterMs: 250
					},
					{
						atMs: 6_000,
						text: 'Hold a directional control to move PTZ continuously, then select Stop PTZ to stop movement explicitly.',
						pauseAfterMs: 250
					},
					{
						atMs: 12_500,
						text: 'Select a device preset such as Driveway gate to move directly to a saved camera position.',
						pauseAfterMs: 250
					},
					{
						atMs: 18_500,
						text: 'Toggling on-device motion detection writes the setting to the camera and verifies it with read-back.',
						pauseAfterMs: 250
					},
					{
						atMs: 24_500,
						text: 'Browser Talk is available only to Administrators, on cameras whose metadata confirms a speaker.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 30_000,
			viewport: { width: 1280, height: 800 },
			captions: [
				{ atMs: 0, text: 'Inspect Front Door health, media profiles, and live preview.' },
				{ atMs: 6_000, text: 'Hold directional PTZ controls and select Stop PTZ explicitly.' },
				{ atMs: 12_500, text: 'Recall saved camera preset Driveway gate.' },
				{ atMs: 18_500, text: 'Toggle hardware motion detection and confirm verified read-back.' },
				{ atMs: 24_500, text: 'Start browser Talk as Administrator on a confirmed speaker.' }
			],
			actions: [
				{
					kind: 'pointer-drag',
					atMs: 6_000,
					selector: '[data-camera-ptz-control="desktop"] button[aria-label="Pan right"]',
					deltaX: 0,
					deltaY: 0,
					durationMs: 900,
					steps: 6
				},
				{
					kind: 'click',
					atMs: 8_800,
					selector: '[data-camera-ptz-control="desktop"] button[aria-label="Stop PTZ"]'
				},
				{
					kind: 'click',
					atMs: 12_500,
					selector: '[data-camera-ptz-control="desktop"] button:has-text("Driveway gate")'
				},
				{
					kind: 'click',
					atMs: 18_500,
					selector: 'input[type="checkbox"][role="switch"][aria-label="Motion detection enabled"]'
				},
				{
					kind: 'click',
					atMs: 24_500,
					selector: '[data-demo-action="toggle-talkback"]'
				}
			],
			completionSignal: {
				selector: '[data-demo-camera-controls-complete="true"]',
				state: 'visible'
			}
		}
	},
	previewScenarioId: 'camera.desktop.details-ptz',
	storySource: 'visual-harness/stories/DemoCameraControls.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoCameraControlsStory.svelte',
		'visual-harness/stories/DemoCameraControls.stories.ts',
		'src/lib/components/CameraOverview.svelte',
		'src/lib/components/CameraPtzControl.svelte',
		'src/lib/components/DesktopPaperRail.svelte'
	]
} as const satisfies DemoScenarioDefinition;

export const liveWallEditorDemo = {
	metadata: {
		storyId: 'demos-live-wall--layout-editor',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '15P-0',
			frameId: '15Z-0',
			scenarioId: 'peek.desktop.layout-editor'
		},
		demo: {
			assetId: 'peek.desktop.layout-editor',
			title: 'Edit dashboard layouts, wall display, and audience scope',
			purpose:
				'Arrange a 12-column live dashboard, tune per-dashboard Wall display settings, and scope who can view the saved dashboard.',
			narration: {
				voice: 'coral',
				instructions: 'Use a calm, concise product-demo voice with clear pacing.',
				cues: [
					{
						atMs: 0,
						text: 'Custom dashboards arrange cameras on a twelve-column snap grid with presets and activity focus.',
						pauseAfterMs: 250
					},
					{
						atMs: 2_000,
						text: 'Apply a preset, pin the primary camera in place, and add remaining cameras before saving.',
						pauseAfterMs: 250
					},
					{
						atMs: 8_800,
						text: 'Open Wall display settings to adjust spacing, media fit, and concurrent browser stream limits.',
						pauseAfterMs: 250
					},
					{
						atMs: 14_400,
						text: 'Dashboard audience controls choose Everyone or named Users while per-user camera grants stay enforced.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 18_000,
			viewport: { width: 1280, height: 800 },
			captions: [
				{ atMs: 0, text: 'Edit a 12-column dashboard layout with presets and Activity focus.' },
				{ atMs: 2_000, text: 'Apply the 1+5 preset, pin Front Door, and add Yard PTZ.' },
				{ atMs: 7_200, text: 'Save the layout to return to the live wall.' },
				{
					atMs: 8_800,
					text: 'Adjust Wall display settings: Hairline gap, Cover fit, and stream limit.'
				},
				{ atMs: 14_400, text: 'Set dashboard audience scope to Everyone or named Users.' }
			],
			actions: [
				{
					kind: 'click',
					atMs: 2_000,
					selector: '[aria-label="Layout presets"] button:has-text("1+5")'
				},
				{ kind: 'click', atMs: 3_800, selector: 'button:has-text("Front Door can be promoted")' },
				{ kind: 'click', atMs: 5_400, selector: 'button[aria-label="Add Yard PTZ to layout"]' },
				{
					kind: 'click',
					atMs: 7_200,
					selector: '[data-peek-layout-editor] header button:has-text("Done")'
				},
				{ kind: 'click', atMs: 8_800, selector: 'button[aria-label="Wall display settings"]' },
				{
					kind: 'click',
					atMs: 10_000,
					selector: 'div[aria-label="Wall display settings"] label:has-text("Hairline")'
				},
				{
					kind: 'click',
					atMs: 11_000,
					selector: 'div[aria-label="Wall display settings"] label:has-text("Cover (cropped)")'
				},
				{
					kind: 'fill',
					atMs: 12_000,
					selector:
						'div[aria-label="Wall display settings"] label:has-text("Stream limit") input[type="number"]',
					value: '6'
				},
				{ kind: 'click', atMs: 13_000, selector: 'button[aria-label="Save display settings"]' },
				{ kind: 'press', atMs: 13_800, key: 'Escape' },
				{ kind: 'click', atMs: 14_400, selector: '[data-demo-action="open-dashboard-access"]' },
				{
					kind: 'click',
					atMs: 15_400,
					selector:
						'dialog[aria-labelledby="peek-dashboard-access-title"] label:has-text("Everyone with KeepPeek access") input[type="checkbox"]'
				},
				{
					kind: 'click',
					atMs: 16_200,
					selector:
						'dialog[aria-labelledby="peek-dashboard-access-title"] label:has-text("Night Guard") input[type="checkbox"]'
				},
				{
					kind: 'click',
					atMs: 17_000,
					selector:
						'dialog[aria-labelledby="peek-dashboard-access-title"] button:has-text("Save access")'
				}
			],
			completionSignal: { selector: '[data-demo-live-wall-complete="true"]', state: 'visible' }
		}
	},
	previewScenarioId: 'peek.desktop.layout-editor',
	storySource: 'visual-harness/stories/DemoLiveWallEditor.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoLiveWallEditorStory.svelte',
		'visual-harness/stories/DemoLiveWallEditor.stories.ts',
		'src/lib/components/PeekLayoutEditor.svelte',
		'src/lib/components/PeekWallSettings.svelte',
		'src/lib/components/PeekDashboardAccessDialog.svelte',
		'src/lib/components/PeekDashboardAudiencePicker.svelte',
		'src/lib/components/DesktopPaperRail.svelte'
	]
} as const satisfies DemoScenarioDefinition;

export const onboardingDemoScenarios: readonly DemoScenarioDefinition[] = [
	firstRunDemo,
	accessControlDemo,
	cameraControlsDemo,
	liveWallEditorDemo
];
