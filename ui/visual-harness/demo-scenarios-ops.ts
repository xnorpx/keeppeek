import type { DemoScenarioDefinition } from '../src/lib/storybook/demo';

export const healthDiagnosisDemo = {
	metadata: {
		storyId: 'demos-camera-health--triage-and-diagnosis',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '2GO-0',
			frameId: '2GY-0',
			scenarioId: 'health.desktop.overview'
		},
		demo: {
			assetId: 'health.desktop.triage-diagnosis',
			title: 'Triage fleet health and diagnose a camera',
			purpose:
				'Show server-authoritative health triage on /system-health, drilling into an offline camera for missing transport evidence, then switching a UDP-dropping camera to TCP.',
			narration: {
				voice: 'coral',
				instructions:
					'Use a calm, authoritative product-demo voice focused on server-observed evidence.',
				cues: [
					{
						atMs: 0,
						text: 'System Health reports server-computed fleet status across transport, frames, decode, and recording.',
						pauseAfterMs: 250
					},
					{
						atMs: 3_200,
						text: 'Select Back Yard from priority findings to inspect its missing transport and stream evidence.',
						pauseAfterMs: 250
					},
					{
						atMs: 7_600,
						text: 'Return to Health and open Porch, which stays connected on UDP while dropping frames.',
						pauseAfterMs: 250
					},
					{
						atMs: 13_400,
						text: 'Use Test TCP to switch RTSP transport to TCP without altering other camera settings.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 18_000,
			viewport: { width: 1280, height: 800 },
			captions: [
				{
					atMs: 0,
					text: 'Server-authoritative health separates transport, frames, decode, and recording.'
				},
				{ atMs: 3_200, text: 'Drill into Back Yard to inspect missing transport evidence.' },
				{ atMs: 7_600, text: 'Switch to Porch to review UDP frame-drop counters.' },
				{ atMs: 13_400, text: 'Test TCP changes RTSP transport without touching other settings.' }
			],
			actions: [
				{
					kind: 'click',
					atMs: 3_200,
					selector: 'a[aria-label="Diagnose Back Yard from findings"]'
				},
				{
					kind: 'click',
					atMs: 7_600,
					selector: 'a[aria-label="Back to Health"]'
				},
				{
					kind: 'click',
					atMs: 9_400,
					selector: 'a[aria-label="Diagnose Porch from findings"]'
				},
				{
					kind: 'click',
					atMs: 13_400,
					selector: '[data-camera-diagnosis-body] ol li:nth-child(3) button'
				}
			],
			completionSignal: {
				selector: '[data-desktop-camera-diagnosis] [role="status"]',
				state: 'visible'
			}
		}
	},
	previewScenarioId: 'health.desktop.overview',
	storySource: 'visual-harness/stories/DemoHealthDiagnosis.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoHealthDiagnosisStory.svelte',
		'visual-harness/stories/DemoHealthDiagnosis.stories.ts',
		'src/lib/components/DesktopHealthOverview.svelte',
		'src/lib/components/DesktopCameraDiagnosis.svelte',
		'src/lib/components/DesktopPaperRail.svelte',
		'src/lib/health-presentation.ts',
		'e2e/fixtures/diagnosis.ts'
	]
} as const satisfies DemoScenarioDefinition;

export const fleetConfigDemo = {
	metadata: {
		storyId: 'demos-fleet-config--templates-and-bulk-apply',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '1NF-0',
			frameId: '1NP-0',
			scenarioId: 'cameras.desktop.fleet'
		},
		demo: {
			assetId: 'cameras.desktop.fleet-config',
			title: 'Manage camera templates and bulk fleet changes',
			purpose:
				'Show camera selection on /cameras, effective policy inheritance in FleetConfigurationManager, secret-referencing templates, server-owned bulk diff preview, and per-camera activation status.',
			narration: {
				voice: 'coral',
				instructions:
					'Use a clear, steady product-demo voice explaining inheritance, secret references, and previewed bulk changes.',
				cues: [
					{
						atMs: 0,
						text: 'Start in Cameras and select the devices you want to update together.',
						pauseAfterMs: 250
					},
					{
						atMs: 4_000,
						text: 'Fleet configuration opens on Overview, showing shared defaults, camera overrides, and effective provenance.',
						pauseAfterMs: 250
					},
					{
						atMs: 7_600,
						text: 'Templates carry reusable recording policies and secret references without storing plaintext credentials.',
						pauseAfterMs: 250
					},
					{
						atMs: 11_600,
						text: 'In Bulk, target the selected cameras with a template and request an authoritative server preview.',
						pauseAfterMs: 250
					},
					{
						atMs: 15_200,
						text: 'Review the exact field diff and reconnect impact, then apply the plan and confirm per-camera activation.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 20_000,
			viewport: { width: 1280, height: 800 },
			captions: [
				{ atMs: 0, text: 'Select target cameras in the fleet list.' },
				{
					atMs: 4_000,
					text: 'Overview shows shared defaults, overrides, and effective provenance.'
				},
				{ atMs: 7_600, text: 'Templates store reusable fields and {secret:KEY} references.' },
				{
					atMs: 11_600,
					text: 'Target selected cameras with a template and preview the server diff.'
				},
				{ atMs: 15_200, text: 'Apply the exact plan and verify per-camera worker activation.' }
			],
			actions: [
				{
					kind: 'click',
					atMs: 1_400,
					selector: '[data-fleet-row="front-door"] > div:last-child input[type="checkbox"]'
				},
				{
					kind: 'click',
					atMs: 2_500,
					selector: '[data-fleet-row="driveway"] > div:last-child input[type="checkbox"]'
				},
				{
					kind: 'click',
					atMs: 4_000,
					selector: '[data-demo-action="open-fleet-config"]'
				},
				{
					kind: 'click',
					atMs: 7_600,
					selector: '#configuration-tab-templates'
				},
				{
					kind: 'click',
					atMs: 9_400,
					selector: '#configuration-panel article:first-of-type button:first-of-type'
				},
				{
					kind: 'click',
					atMs: 11_600,
					selector: '#configuration-tab-bulk'
				},
				{
					kind: 'select',
					atMs: 12_800,
					selector: '#bulk-target-mode',
					value: 'selected'
				},
				{
					kind: 'select',
					atMs: 13_800,
					selector: '#bulk-change-mode',
					value: 'template'
				},
				{
					kind: 'click',
					atMs: 15_200,
					selector: '#configuration-panel button:has-text("Preview exact changes")'
				},
				{
					kind: 'click',
					atMs: 17_400,
					selector:
						'#configuration-panel section[aria-labelledby="configuration-review-heading"] button:last-of-type'
				}
			],
			completionSignal: { selector: '#configuration-activation-heading', state: 'visible' }
		}
	},
	previewScenarioId: 'cameras.desktop.fleet',
	storySource: 'visual-harness/stories/DemoFleetConfig.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoFleetConfigStory.svelte',
		'visual-harness/stories/DemoFleetConfig.stories.ts',
		'src/lib/components/CameraFleetRow.svelte',
		'src/lib/components/FleetConfigurationManager.svelte',
		'src/lib/components/DesktopPaperRail.svelte',
		'src/lib/camera-fleet.ts'
	]
} as const satisfies DemoScenarioDefinition;

export const configZipDemo = {
	metadata: {
		storyId: 'demos-config-zip--export-stage-and-reject',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '39R-0',
			frameId: 'ANO-0',
			scenarioId: 'settings.desktop.appearance-system-logs'
		},
		demo: {
			assetId: 'settings.desktop.config-zip',
			title: 'Export, stage, and validate a configuration ZIP',
			purpose:
				'Show Settings Backup and restore exporting a two-file format-3 configuration ZIP, gating local apply behind explicit confirmation, staging until restart, and rejecting a modified archive without touching live files.',
			narration: {
				voice: 'coral',
				instructions:
					'Use a calm, deliberate product-demo voice highlighting plaintext-secret handling and staged restart safety.',
				cues: [
					{
						atMs: 0,
						text: 'Backup and restore exports your configuration and secrets files as one checksummed ZIP, without touching recordings.',
						pauseAfterMs: 250
					},
					{
						atMs: 4_400,
						text: 'Choosing a valid archive keeps Apply configuration locked until you confirm replacement on restart.',
						pauseAfterMs: 250
					},
					{
						atMs: 8_200,
						text: 'Applying validates checksums and stages the files safely until you choose Restart to apply.',
						pauseAfterMs: 250
					},
					{
						atMs: 13_000,
						text: 'A modified archive is rejected before staging, leaving live files untouched and preserving your selection for retry.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 18_000,
			viewport: { width: 1280, height: 800 },
			captions: [
				{ atMs: 0, text: 'Export config.toml and secrets.toml without interrupting recording.' },
				{ atMs: 4_400, text: 'Apply stays gated until replacement on restart is confirmed.' },
				{ atMs: 8_200, text: 'Valid archives stage safely and wait for Restart to apply.' },
				{ atMs: 13_000, text: 'Tampered archives fail validation without modifying live files.' }
			],
			actions: [
				{
					kind: 'click',
					atMs: 2_000,
					selector: '#backups button:has-text("Export ZIP")'
				},
				{
					kind: 'click',
					atMs: 4_400,
					selector: '[data-demo-action="attach-valid-zip"]'
				},
				{
					kind: 'click',
					atMs: 6_800,
					selector: '#backups input[type="checkbox"]'
				},
				{
					kind: 'click',
					atMs: 8_200,
					selector: '#backups button:has-text("Apply configuration")'
				},
				{
					kind: 'click',
					atMs: 11_400,
					selector: '#backups button:has-text("Restart to apply")'
				},
				{
					kind: 'click',
					atMs: 13_000,
					selector: '[data-demo-action="attach-invalid-zip"]'
				},
				{
					kind: 'click',
					atMs: 14_400,
					selector: '#backups input[type="checkbox"]'
				},
				{
					kind: 'click',
					atMs: 15_400,
					selector: '#backups button:has-text("Apply configuration")'
				}
			],
			completionSignal: { selector: '#backups [role="alert"]', state: 'visible' }
		}
	},
	previewScenarioId: 'settings.desktop.appearance-system-logs',
	storySource: 'visual-harness/stories/DemoConfigZip.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoConfigZipStory.svelte',
		'visual-harness/stories/DemoConfigZip.stories.ts',
		'src/lib/components/BackupRestoreSection.svelte',
		'src/lib/components/DesktopPaperRail.svelte',
		'src/lib/backup-http-client.ts'
	]
} as const satisfies DemoScenarioDefinition;

export const recordingMaintenanceDemo = {
	metadata: {
		storyId: 'demos-recording-maintenance--delete-and-reconcile',
		paper: {
			fileId: '01M0B0VBH78TMTX40GCYYQ37SG',
			tokenHash: 'cf3b1cd7',
			boardId: '233-0',
			frameId: '23D-0',
			scenarioId: 'settings.desktop.storage-retention'
		},
		demo: {
			assetId: 'recordings.desktop.maintenance',
			title: 'Preview recording deletion and reconcile catalog drift',
			purpose:
				'Show plan-then-confirm whole-recording deletion with durable Queued to Staged to Deleted ledger states, followed by bounded catalog reconciliation and playback-index rebuild.',
			narration: {
				voice: 'coral',
				instructions:
					'Use a measured, precise product-demo voice emphasizing whole-recording boundaries, explicit confirmation, and safe catalog remedies.',
				cues: [
					{
						atMs: 0,
						text: 'Recording maintenance starts with a scope preview for the selected camera, stream, and UTC interval.',
						pauseAfterMs: 250
					},
					{
						atMs: 2_800,
						text: 'Review the exact whole-recording boundaries, total bytes, bookmarks, and related exports before typing DELETE 2.',
						pauseAfterMs: 250
					},
					{
						atMs: 6_800,
						text: 'Confirmed deletions progress through a durable Queued, Staged, and Deleted ledger.',
						pauseAfterMs: 250
					},
					{
						atMs: 11_400,
						text: 'Switch to Reconciliation and inspect the catalog against archive files without mutating media.',
						pauseAfterMs: 250
					},
					{
						atMs: 14_800,
						text: 'Filter to an index mismatch and confirm Rebuild index to repair playback metadata while leaving media bytes intact.',
						pauseAfterMs: 300
					}
				]
			},
			durationMs: 20_000,
			viewport: { width: 1280, height: 800 },
			captions: [
				{ atMs: 0, text: 'Select camera, stream, and UTC bounds, then preview deletion.' },
				{ atMs: 2_800, text: 'Inspect whole-recording boundaries and confirm with DELETE 2.' },
				{ atMs: 6_800, text: 'Track the durable ledger from Queued to Staged to Deleted.' },
				{ atMs: 11_400, text: 'Run a bounded read-only catalog inspection under Reconciliation.' },
				{ atMs: 14_800, text: 'Filter findings and rebuild an eligible playback index safely.' }
			],
			actions: [
				{
					kind: 'select',
					atMs: 1_600,
					selector: '[data-demo-action="select-stream"]',
					value: 'main'
				},
				{
					kind: 'click',
					atMs: 2_800,
					selector: '[data-demo-action="preview-deletion"]'
				},
				{
					kind: 'fill',
					atMs: 5_600,
					selector: '[data-demo-action="confirm-delete-input"]',
					value: 'DELETE 2'
				},
				{
					kind: 'click',
					atMs: 6_800,
					selector: '[data-demo-action="confirm-delete"]'
				},
				{
					kind: 'click',
					atMs: 11_400,
					selector: '[data-demo-action="tab-reconciliation"]'
				},
				{
					kind: 'click',
					atMs: 12_600,
					selector: '[aria-label="Catalog reconciliation"] button:has-text("Inspect catalog")'
				},
				{
					kind: 'select',
					atMs: 14_800,
					selector: '[aria-label="Catalog reconciliation"] select',
					// Proto enum value 12 selects playback-index mismatch findings.
					value: '12'
				},
				{
					kind: 'click',
					atMs: 16_200,
					selector: '[aria-label="Catalog reconciliation"] button:has-text("Rebuild index")'
				},
				{
					kind: 'click',
					atMs: 17_600,
					selector: 'dialog[aria-labelledby="reconcile-title"] button:has-text("Rebuild index")'
				}
			],
			completionSignal: { selector: '[data-demo-maintenance-complete="true"]', state: 'visible' }
		}
	},
	previewScenarioId: 'settings.desktop.storage-retention',
	storySource: 'visual-harness/stories/DemoRecordingMaintenance.stories.ts',
	fixtureSources: [
		'visual-harness/stories/DemoRecordingMaintenanceStory.svelte',
		'visual-harness/stories/DemoRecordingMaintenance.stories.ts',
		'src/lib/components/RecordingReconciliation.svelte',
		'src/lib/components/DesktopPaperRail.svelte',
		'src/lib/control-client-maintenance.ts'
	]
} as const satisfies DemoScenarioDefinition;

export const opsDemoScenarios: readonly DemoScenarioDefinition[] = [
	healthDiagnosisDemo,
	fleetConfigDemo,
	configZipDemo,
	recordingMaintenanceDemo
];
