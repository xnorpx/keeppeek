import type { DemoScenarioDefinition } from '../src/lib/storybook/demo';

export const notificationsMqttDemo = {
	metadata: {
		storyId: 'demos-notifications-mqtt--rules-push-and-mqtt',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '2YM-0',
			frameId: '2YW-0',
			scenarioId: 'settings.desktop.notifications'
		},
		demo: {
			assetId: 'settings.desktop.notifications-mqtt',
			title: 'Configure notification rules, mobile push, and MQTT 5',
			purpose:
				'Show active notification rules with Pushover delivery and suppression history, bounded Keeppeek Service mobile push signaling, and MQTT 5 forwarder health and testing.',
			narration: {
				voice: 'coral',
				instructions: 'Use a calm, concise product-demo voice with clear transitions.',
				cues: [
					{
						atMs: 0,
						text: 'Notification rules run independently from camera ingest, showing active channels and suppression totals.',
						pauseAfterMs: 250
					},
					{
						atMs: 2_600,
						text: 'The Inbox tracks unread alerts per account, while History records every delivery and cooldown decision.',
						pauseAfterMs: 250
					},
					{
						atMs: 6_600,
						text: 'Expand any entry to compare collapsed or suppressed decisions against Pushover request receipts.',
						pauseAfterMs: 250
					},
					{
						atMs: 9_400,
						text: 'Keeppeek Service adds optional mobile push using a compact 109-byte signed signal with no event metadata.',
						pauseAfterMs: 300
					},
					{
						atMs: 13_600,
						text: 'With signaling enabled and saved, send a test signal to confirm the service accepts this server.',
						pauseAfterMs: 250
					},
					{
						atMs: 15_600,
						text: 'For automation systems, the MQTT 5 forwarder shows broker topics, delivery guarantees, outbox depth, and connection testing.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 20_000,
			viewport: { width: 1280, height: 800 },
			captions: [
				{
					atMs: 0,
					text: 'Inspect active notification rules, Pushover channels, and suppression counts.'
				},
				{ atMs: 2_600, text: 'Review unread alerts in the per-principal Inbox.' },
				{ atMs: 5_200, text: 'Inspect delivered, collapsed, and suppressed outcomes in History.' },
				{ atMs: 9_400, text: 'Enable optional Keeppeek Service mobile push signaling.' },
				{ atMs: 13_600, text: 'Send a test signal to confirm service admission.' },
				{
					atMs: 15_600,
					text: 'Verify MQTT 5 delivery settings, outbox health, and broker connection.'
				}
			],
			actions: [
				{
					kind: 'click',
					atMs: 2_600,
					selector: '#notifications button[role="tab"]:has-text("Inbox")'
				},
				{
					kind: 'click',
					atMs: 5_200,
					selector: '#notifications button[role="tab"]:has-text("History")'
				},
				{ kind: 'click', atMs: 6_600, selector: '#notifications details:first-of-type summary' },
				{ kind: 'click', atMs: 9_400, selector: '[data-demo-action="tab-cloud"]' },
				{
					kind: 'click',
					atMs: 11_000,
					selector: 'label:has-text("Enable Keeppeek Service signaling") input[type="checkbox"]'
				},
				{ kind: 'click', atMs: 12_200, selector: 'button:has-text("Save cloud settings")' },
				{ kind: 'click', atMs: 13_600, selector: 'button:has-text("Send test signal")' },
				{ kind: 'click', atMs: 15_600, selector: '[data-demo-action="tab-mqtt"]' },
				{
					kind: 'click',
					atMs: 17_000,
					selector: '[data-mqtt-integration] button:has-text("Edit broker")'
				},
				{
					kind: 'click',
					atMs: 18_400,
					selector: '[data-mqtt-editor] button:has-text("Test connection")'
				}
			],
			completionSignal: { selector: '[data-mqtt-editor] [role="status"]', state: 'visible' }
		}
	},
	previewScenarioId: 'settings.desktop.notifications',
	storySource: 'visual-harness/stories/DemoNotificationsMqtt.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoNotificationsMqttStory.svelte',
		'visual-harness/stories/DemoNotificationsMqtt.stories.ts',
		'src/lib/components/NotificationsRuntime.svelte',
		'src/lib/components/CloudNotificationSettings.svelte',
		'src/lib/components/MqttIntegrationPanel.svelte'
	]
} as const satisfies DemoScenarioDefinition;

export const homeAssistantCardDemo = {
	metadata: {
		storyId: 'demos-home-assistant--card-and-editor',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '2UB-0',
			frameId: '2UL-0',
			scenarioId: 'settings.desktop.integrations'
		},
		demo: {
			assetId: 'home-assistant.desktop.card',
			title: 'Configure the direct Home Assistant Lovelace card',
			purpose:
				'Show the Home Assistant visual editor discovering sources, updating grid columns and layout live, and focusing or restoring cameras on the direct WebRTC card.',
			narration: {
				voice: 'coral',
				instructions: 'Use a calm, concise product-demo voice with clear transitions.',
				cues: [
					{
						atMs: 0,
						text: 'The Home Assistant card streams live WebRTC video directly from KeepPeek without a media proxy.',
						pauseAfterMs: 250
					},
					{
						atMs: 6_200,
						text: 'Select Load sources in the visual editor to discover permitted camera IDs without starting extra streams.',
						pauseAfterMs: 250
					},
					{
						atMs: 12_800,
						text: 'Adjust columns or switch between grid and single-camera layouts and watch the Lovelace preview update immediately.',
						pauseAfterMs: 300
					},
					{
						atMs: 19_500,
						text: 'Switch back to a two-column grid to keep multiple entrances visible side by side.',
						pauseAfterMs: 250
					},
					{
						atMs: 25_000,
						text: 'Focus any live tile to inspect one camera full width, then select Show all cameras to return to the grid.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 33_000,
			viewport: { width: 1280, height: 800 },
			captions: [
				{ atMs: 0, text: 'The Lovelace card connects directly from the browser to KeepPeek.' },
				{
					atMs: 6_200,
					text: 'Load sources discovers permitted cameras without subscribing to video.'
				},
				{ atMs: 12_800, text: 'Change columns and switch between Grid and Single camera live.' },
				{ atMs: 19_500, text: 'Return to a two-column Grid layout.' },
				{ atMs: 25_000, text: 'Focus a single tile, then return to all cameras.' }
			],
			actions: [
				{ kind: 'click', atMs: 6_200, selector: 'button:has-text("Load sources")' },
				{ kind: 'select', atMs: 12_800, selector: 'label:has-text("Columns") select', value: '1' },
				{
					kind: 'click',
					atMs: 15_400,
					selector: 'label:has-text("Single camera") input[type="radio"]'
				},
				{ kind: 'click', atMs: 19_500, selector: 'label:has-text("Grid") input[type="radio"]' },
				{ kind: 'select', atMs: 21_500, selector: 'label:has-text("Columns") select', value: '2' },
				{ kind: 'click', atMs: 25_000, selector: 'button[aria-label="Focus Front door"]' },
				{ kind: 'click', atMs: 28_500, selector: 'button[aria-label="Show all cameras"]' }
			],
			completionSignal: { selector: '[data-demo-ha-complete="true"]', state: 'visible' }
		}
	},
	previewScenarioId: 'settings.desktop.integrations',
	storySource: 'visual-harness/stories/DemoHomeAssistantCard.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoHomeAssistantCardStory.svelte',
		'visual-harness/stories/DemoHomeAssistantCard.stories.ts',
		'src/lib/home-assistant/Card.svelte',
		'src/lib/home-assistant/Editor.svelte',
		'src/lib/home-assistant/VideoTile.svelte',
		'src/lib/home-assistant/card.css'
	]
} as const satisfies DemoScenarioDefinition;

export const logsDiagnosticsDemo = {
	metadata: {
		storyId: 'demos-logs-diagnostics--appearance-logs-and-bug-report',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '39R-0',
			frameId: '3A1-0',
			scenarioId: 'settings.desktop.appearance-system-logs'
		},
		demo: {
			assetId: 'settings.desktop.logs-diagnostics',
			title: 'Inspect appearance, live logs, diagnostics, and bug reports',
			purpose:
				'Show theme switching and runtime details, live log filtering and pausing, scrubbed diagnostics export, and the in-app bug reporting review workflow.',
			narration: {
				voice: 'coral',
				instructions: 'Use a calm, concise product-demo voice with clear transitions.',
				cues: [
					{
						atMs: 0,
						text: 'Appearance, System, and Logs keep browser theme preferences separate from server runtime evidence.',
						pauseAfterMs: 250
					},
					{
						atMs: 2_000,
						text: 'Switch between dark and light themes instantly while inspecting the running server version and uptime.',
						pauseAfterMs: 250
					},
					{
						atMs: 5_000,
						text: 'Open Logs to filter live entries by severity and target, then pause the tail to inspect a warning.',
						pauseAfterMs: 250
					},
					{
						atMs: 10_200,
						text: 'Download diagnostics prepares a scrubbed archive with retained logs, health, and runtime metrics.',
						pauseAfterMs: 250
					},
					{
						atMs: 11_800,
						text: 'Select Report a bug to capture logs only or a video-only screen recording while reproducing an issue.',
						pauseAfterMs: 300
					},
					{
						atMs: 15_200,
						text: 'Stop capture to review counts, warnings, and automatically redacted issue text before sharing.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 22_000,
			viewport: { width: 1280, height: 800 },
			captions: [
				{ atMs: 0, text: 'Inspect runtime version and switch between dark and light themes.' },
				{ atMs: 5_000, text: 'Filter live logs by level and target, then pause the stream.' },
				{ atMs: 10_200, text: 'Prepare a scrubbed diagnostics package.' },
				{
					atMs: 11_800,
					text: 'Start a bug report with logs only or video-only screen capture.'
				},
				{
					atMs: 15_200,
					text: 'Review scrubbed issue text, log counts, and warnings before sharing.'
				}
			],
			actions: [
				{
					kind: 'click',
					atMs: 2_000,
					selector: '[role="group"][aria-label="Theme preference"] button:has-text("Light")'
				},
				{
					kind: 'click',
					atMs: 3_600,
					selector: '[role="group"][aria-label="Theme preference"] button:has-text("Dark")'
				},
				{ kind: 'click', atMs: 5_000, selector: '[data-demo-action="view-logs"]' },
				{
					kind: 'click',
					atMs: 6_400,
					selector: 'fieldset label:has-text("Info") input[type="checkbox"]'
				},
				{ kind: 'fill', atMs: 7_600, selector: '#log-target-filter', value: 'rtsp' },
				{ kind: 'click', atMs: 8_800, selector: 'button[aria-label="Pause"]' },
				{ kind: 'click', atMs: 10_200, selector: '[data-demo-action="download-diagnostics"]' },
				{ kind: 'click', atMs: 11_800, selector: 'button:has-text("Report a bug")' },
				{
					kind: 'click',
					atMs: 13_800,
					selector: 'dialog[open] button:has-text("Collect logs only")'
				},
				{
					kind: 'click',
					atMs: 15_200,
					selector: 'aside[aria-label="Bug report in progress"] button:has-text("Stop")'
				},
				{
					kind: 'fill',
					atMs: 16_200,
					selector: 'dialog[open] label:has-text("Issue title") input',
					value: 'Porch RTSP gap on 192.0.2.44'
				},
				{
					kind: 'fill',
					atMs: 16_800,
					selector: 'dialog[open] label:has-text("Problem description") textarea',
					value: 'RTP sequence gap observed on porch-cam during reconnect.'
				},
				{
					kind: 'fill',
					atMs: 17_400,
					selector: 'dialog[open] label:has-text("Steps to reproduce") textarea',
					value: 'Open Logs, filter target to rtsp, and pause tail.'
				},
				{
					kind: 'fill',
					atMs: 18_000,
					selector: 'dialog[open] label:has-text("Expected behavior") textarea',
					value: 'Continuous RTP packets without sequence gaps.'
				},
				{
					kind: 'fill',
					atMs: 18_600,
					selector: 'dialog[open] label:has-text("Actual behavior") textarea',
					value: 'WARN porch rtp gap 41022..41029 logged.'
				},
				{
					kind: 'click',
					atMs: 19_400,
					selector: 'dialog[open] button:has-text("Review submission")'
				}
			],
			completionSignal: { selector: 'dialog[open] [data-confirm-report]', state: 'visible' }
		}
	},
	previewScenarioId: 'settings.desktop.appearance-system-logs',
	storySource: 'visual-harness/stories/DemoLogsDiagnostics.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoLogsDiagnosticsStory.svelte',
		'visual-harness/stories/DemoLogsDiagnostics.stories.ts',
		'src/lib/components/AppearanceSystemSection.svelte',
		'src/lib/components/logging/LogViewer.svelte',
		'src/lib/components/ReportBugWorkflow.svelte'
	]
} as const satisfies DemoScenarioDefinition;

export const integrationsDemoScenarios: readonly DemoScenarioDefinition[] = [
	notificationsMqttDemo,
	homeAssistantCardDemo,
	logsDiagnosticsDemo
];
